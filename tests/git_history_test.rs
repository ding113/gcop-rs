//! Integration tests for `GitRepository::get_commit_history_full`.
//!
//! Verify body extraction, subject/body split, and empty-repo handling
//! against real on-disk git repositories created via `git2`.

use gcop_rs::error::Result;
use gcop_rs::git::{GitOperations, repository::GitRepository};
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

fn write_file(repo_path: &Path, name: &str, content: &str) -> Result<()> {
    fs::write(repo_path.join(name), content)?;
    Ok(())
}

fn stage(repo: &git2::Repository, name: &str) -> Result<()> {
    let mut index = repo.index()?;
    index.add_path(Path::new(name))?;
    index.write()?;
    Ok(())
}

fn commit_with_message(
    repo: &git2::Repository,
    message: &str,
    parents: Vec<&git2::Commit>,
) -> Result<git2::Oid> {
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = git2::Signature::now("Test User", "test@example.com")?;
    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
    Ok(oid)
}

/// Like `commit_with_message` but stamps the commit at a deterministic UNIX
/// time so revwalk's `Sort::TIME` ordering is stable across same-second creation.
fn commit_at_time(
    repo: &git2::Repository,
    message: &str,
    parents: Vec<&git2::Commit>,
    epoch_seconds: i64,
) -> Result<git2::Oid> {
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let time = git2::Time::new(epoch_seconds, 0);
    let sig = git2::Signature::new("Test User", "test@example.com", &time)?;
    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
    Ok(oid)
}

#[test]
#[serial]
fn test_get_commit_history_full_extracts_body() -> Result<()> {
    let temp = TempDir::new()?;
    let repo_path = temp.path();
    let repo = init_git_repo(repo_path)?;

    write_file(repo_path, "a.txt", "v1\n")?;
    stage(&repo, "a.txt")?;
    commit_with_message(
        &repo,
        "feat: x\n\nDetailed explanation\nspanning multiple lines",
        vec![],
    )?;

    let _guard = DirGuard::enter(repo_path)?;
    let git_repo = GitRepository::open(None)?;
    let history = git_repo.get_commit_history_full(300)?;

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].subject, "feat: x");
    assert_eq!(
        history[0].body,
        "Detailed explanation\nspanning multiple lines"
    );
    assert_eq!(history[0].author_email, "test@example.com");
    assert_eq!(history[0].parent_count, 0);
    Ok(())
}

#[test]
#[serial]
fn test_get_commit_history_full_empty_repo() -> Result<()> {
    let temp = TempDir::new()?;
    let _repo = init_git_repo(temp.path())?;
    let _guard = DirGuard::enter(temp.path())?;
    let git_repo = GitRepository::open(None)?;
    let history = git_repo.get_commit_history_full(300)?;
    assert!(history.is_empty());
    Ok(())
}

#[test]
#[serial]
fn test_get_commit_history_full_respects_limit() -> Result<()> {
    let temp = TempDir::new()?;
    let repo_path = temp.path();
    let repo = init_git_repo(repo_path)?;

    // 5 commits: limit=2 should yield exactly the 2 most-recent.
    // Explicit increasing timestamps so revwalk ordering is deterministic.
    let mut prev: Option<git2::Oid> = None;
    for i in 0..5 {
        write_file(repo_path, "a.txt", &format!("v{i}\n"))?;
        stage(&repo, "a.txt")?;
        let parents: Vec<git2::Commit> = match prev {
            Some(p) => vec![repo.find_commit(p)?],
            None => vec![],
        };
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        prev = Some(commit_at_time(
            &repo,
            &format!("chore: commit {i}"),
            parent_refs,
            1_700_000_000 + i as i64,
        )?);
    }

    let _guard = DirGuard::enter(repo_path)?;
    let git_repo = GitRepository::open(None)?;
    let history = git_repo.get_commit_history_full(2)?;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].subject, "chore: commit 4");
    assert_eq!(history[1].subject, "chore: commit 3");
    Ok(())
}

#[test]
#[serial]
fn test_get_commit_history_full_limit_zero_returns_empty() -> Result<()> {
    let temp = TempDir::new()?;
    let repo_path = temp.path();
    let repo = init_git_repo(repo_path)?;
    write_file(repo_path, "a.txt", "v1\n")?;
    stage(&repo, "a.txt")?;
    commit_with_message(&repo, "feat: one", vec![])?;

    let _guard = DirGuard::enter(repo_path)?;
    let git_repo = GitRepository::open(None)?;
    let history = git_repo.get_commit_history_full(0)?;
    assert!(history.is_empty());
    Ok(())
}
