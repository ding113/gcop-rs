//! End-to-end integration tests for the historical commit reference feature.
//!
//! Exercises `gather_reference_messages` against real on-disk git repositories
//! and verifies that the sampled examples land in the prompt produced by
//! `build_commit_prompt_split`.

use gcop_rs::config::HistoryRefConfig;
use gcop_rs::error::Result;
use gcop_rs::git::history::HistoricalCommit;
use gcop_rs::git::{GitOperations, repository::GitRepository};
use gcop_rs::llm::CommitContext;
use gcop_rs::llm::history_sampler::gather_reference_messages;
use gcop_rs::llm::prompt::build_commit_prompt_split;
use serial_test::serial;
use std::env;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

struct DirGuard {
    original: std::path::PathBuf,
}

impl DirGuard {
    fn enter(path: &Path) -> Result<Self> {
        let original = env::current_dir()?;
        env::set_current_dir(path)?;
        Ok(Self { original })
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.original);
    }
}

fn init_git_repo(path: &Path) -> Result<git2::Repository> {
    git2::Repository::init(path).map_err(gcop_rs::error::GcopError::from)
}

fn commit_at_time(
    repo: &git2::Repository,
    repo_path: &Path,
    file: &str,
    contents: &str,
    message: &str,
    parents: Vec<&git2::Commit>,
    epoch_seconds: i64,
) -> Result<git2::Oid> {
    fs::write(repo_path.join(file), contents)?;
    let mut index = repo.index()?;
    index.add_path(Path::new(file))?;
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let time = git2::Time::new(epoch_seconds, 0);
    let sig = git2::Signature::new("Test User", "test@example.com", &time)?;
    Ok(repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?)
}

#[test]
#[serial]
fn test_gather_reference_messages_basic() -> Result<()> {
    let temp = TempDir::new()?;
    let repo_path = temp.path();
    let repo = init_git_repo(repo_path)?;

    let base = 1_700_000_000;
    let h1 = commit_at_time(&repo, repo_path, "a.txt", "v1", "feat: a", vec![], base)?;
    let h2 = commit_at_time(
        &repo,
        repo_path,
        "a.txt",
        "v2",
        "chore: b",
        vec![&repo.find_commit(h1)?],
        base + 1,
    )?;
    let h3 = commit_at_time(
        &repo,
        repo_path,
        "a.txt",
        "v3",
        "random msg",
        vec![&repo.find_commit(h2)?],
        base + 2,
    )?;
    let _h4 = commit_at_time(
        &repo,
        repo_path,
        "a.txt",
        "v4",
        "fix: d",
        vec![&repo.find_commit(h3)?],
        base + 3,
    )?;

    let _guard = DirGuard::enter(repo_path)?;
    let git_repo = GitRepository::open(None)?;

    let cfg = HistoryRefConfig {
        count: 3,
        ..HistoryRefConfig::default()
    };
    let examples = gather_reference_messages(&git_repo, &cfg, None, Some(42));

    assert!(!examples.is_empty(), "should pick some commits");
    assert!(examples.len() <= 3, "should respect count cap");
    // Conventional commits dominate the sample over plain-text ones.
    let joined = examples.join("\n");
    assert!(joined.contains("feat: a") || joined.contains("fix: d"));
    Ok(())
}

#[test]
#[serial]
fn test_gather_reference_messages_empty_repo_returns_empty() -> Result<()> {
    let temp = TempDir::new()?;
    let _repo = init_git_repo(temp.path())?;
    let _guard = DirGuard::enter(temp.path())?;
    let git_repo = GitRepository::open(None)?;

    let cfg = HistoryRefConfig::default();
    let examples = gather_reference_messages(&git_repo, &cfg, None, Some(1));
    assert!(examples.is_empty());
    Ok(())
}

#[test]
#[serial]
fn test_gather_reference_messages_disabled_returns_empty() -> Result<()> {
    let temp = TempDir::new()?;
    let repo_path = temp.path();
    let repo = init_git_repo(repo_path)?;
    commit_at_time(
        &repo,
        repo_path,
        "a.txt",
        "v1",
        "feat: one",
        vec![],
        1_700_000_000,
    )?;

    let _guard = DirGuard::enter(repo_path)?;
    let git_repo = GitRepository::open(None)?;
    let cfg = HistoryRefConfig {
        enabled: false,
        ..HistoryRefConfig::default()
    };
    let examples = gather_reference_messages(&git_repo, &cfg, None, Some(7));
    assert!(examples.is_empty());
    Ok(())
}

#[test]
#[serial]
fn test_commit_pipeline_injects_history_into_prompt() -> Result<()> {
    let temp = TempDir::new()?;
    let repo_path = temp.path();
    let repo = init_git_repo(repo_path)?;

    let base = 1_700_000_000;
    let h1 = commit_at_time(&repo, repo_path, "a.txt", "v1", "feat: alpha", vec![], base)?;
    let _h2 = commit_at_time(
        &repo,
        repo_path,
        "a.txt",
        "v2",
        "feat: bravo",
        vec![&repo.find_commit(h1)?],
        base + 1,
    )?;

    let _guard = DirGuard::enter(repo_path)?;
    let git_repo = GitRepository::open(None)?;

    let cfg = HistoryRefConfig {
        count: 2,
        ..HistoryRefConfig::default()
    };
    let examples = gather_reference_messages(&git_repo, &cfg, None, Some(42));
    assert!(!examples.is_empty());

    // Feed into the actual prompt builder used by the commit flow.
    let ctx = CommitContext {
        historical_examples: examples.clone(),
        ..Default::default()
    };
    let (system, user) = build_commit_prompt_split("diff content", &ctx, None, None);
    assert!(!system.contains("Project commit-style references"));
    assert!(user.contains("Project commit-style references"));
    for ex in &examples {
        assert!(
            user.contains(ex.split('\n').next().unwrap()),
            "user prompt should include sampled subject"
        );
    }
    Ok(())
}

// === Robustness extras ===

/// Mock that always errors out from get_commit_history_full to exercise the
/// error-swallowing path in gather_reference_messages.
struct ErroringRepo;

impl GitOperations for ErroringRepo {
    fn get_staged_diff(&self) -> Result<String> {
        unimplemented!()
    }
    fn get_uncommitted_diff(&self) -> Result<String> {
        unimplemented!()
    }
    fn get_commit_diff(&self, _: &str) -> Result<String> {
        unimplemented!()
    }
    fn get_range_diff(&self, _: &str) -> Result<String> {
        unimplemented!()
    }
    fn get_file_content(&self, _: &str) -> Result<String> {
        unimplemented!()
    }
    fn commit(&self, _: &str) -> Result<()> {
        unimplemented!()
    }
    fn commit_amend(&self, _: &str) -> Result<()> {
        unimplemented!()
    }
    fn get_current_branch(&self) -> Result<Option<String>> {
        unimplemented!()
    }
    fn get_diff_stats(&self, _: &str) -> Result<gcop_rs::git::DiffStats> {
        unimplemented!()
    }
    fn has_staged_changes(&self) -> Result<bool> {
        unimplemented!()
    }
    fn get_commit_history(&self) -> Result<Vec<gcop_rs::git::CommitInfo>> {
        unimplemented!()
    }
    fn get_commit_history_full(&self, _: usize) -> Result<Vec<HistoricalCommit>> {
        Err(gcop_rs::error::GcopError::GitCommand(
            "simulated failure".to_string(),
        ))
    }
    fn get_commit_line_stats(&self, _: &str) -> Result<(usize, usize)> {
        unimplemented!()
    }
    fn is_empty(&self) -> Result<bool> {
        Ok(false)
    }
    fn get_staged_files(&self) -> Result<Vec<String>> {
        unimplemented!()
    }
    fn unstage_all(&self) -> Result<()> {
        unimplemented!()
    }
    fn stage_files(&self, _: &[String]) -> Result<()> {
        unimplemented!()
    }
    fn get_workdir(&self) -> Result<std::path::PathBuf> {
        unimplemented!()
    }
}

#[test]
#[serial]
fn test_gather_reference_messages_error_path_swallows_and_logs() {
    let cfg = HistoryRefConfig::default();
    let examples = gather_reference_messages(&ErroringRepo, &cfg, None, Some(1));
    // Error from get_commit_history_full must not propagate; result is empty.
    assert!(examples.is_empty());
}

#[test]
#[serial]
fn test_gather_reference_messages_no_provider_falls_back_to_default_budget() -> Result<()> {
    let temp = TempDir::new()?;
    let repo_path = temp.path();
    let repo = init_git_repo(repo_path)?;
    commit_at_time(
        &repo,
        repo_path,
        "a.txt",
        "v1",
        "feat: alpha",
        vec![],
        1_700_000_000,
    )?;
    let _guard = DirGuard::enter(repo_path)?;
    let git_repo = GitRepository::open(None)?;
    // No provider supplied -> should still produce non-empty examples,
    // using the placeholder + DEFAULT_CONTEXT_WINDOW × ratio budget.
    let cfg = HistoryRefConfig::default();
    let examples = gather_reference_messages(&git_repo, &cfg, None, Some(7));
    assert_eq!(examples.len(), 1);
    Ok(())
}
