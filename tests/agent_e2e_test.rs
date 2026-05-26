//! End-to-end tests for `gcop-rs agent install/uninstall/status`.
//!
//! Each test spawns the actual gcop-rs binary (via `CARGO_BIN_EXE_gcop-rs`,
//! which Cargo injects for integration tests) with `GCOP_CLAUDE_DIR` and/or
//! `CODEX_HOME` pointing to a `tempfile::TempDir`. The TempDir auto-cleans
//! when the test ends, so tests do not pollute the user's actual home.
//!
//! All tests are `#[serial]` because they mutate process env vars; running
//! them in parallel would race.

use serial_test::serial;
use std::ffi::OsString;
use std::fs;
use std::process::{Command, Output};
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_gcop-rs")
}

/// Run `gcop-rs <args>`, scrubbing env so the host's real ~/.claude /
/// ~/.codex never leaks in.
fn run(
    args: &[&str],
    claude_dir: Option<&std::path::Path>,
    codex_dir: Option<&std::path::Path>,
) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    // Scrub overrides first so callers must pass them explicitly.
    cmd.env_remove("GCOP_CLAUDE_DIR");
    cmd.env_remove("CODEX_HOME");
    if let Some(d) = claude_dir {
        cmd.env("GCOP_CLAUDE_DIR", OsString::from(d.as_os_str()));
    }
    if let Some(d) = codex_dir {
        cmd.env("CODEX_HOME", OsString::from(d.as_os_str()));
    }
    cmd.output().expect("failed to spawn gcop-rs binary")
}

fn assert_success(out: &Output, context: &str) {
    assert!(
        out.status.success(),
        "{context}: gcop-rs failed (exit={:?})\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
#[serial]
fn install_claude_creates_both_skill_and_block() {
    let tmp = TempDir::new().unwrap();
    let out = run(&["agent", "install", "claude"], Some(tmp.path()), None);
    assert_success(&out, "install claude");

    let skill = tmp.path().join("skills").join("gcop").join("SKILL.md");
    let claude_md = tmp.path().join("CLAUDE.md");
    assert!(skill.exists(), "SKILL.md missing");
    assert!(claude_md.exists(), "CLAUDE.md missing");

    // Hard constraints from the plan: every install must produce content
    // that tells the agent to use --split, -y, and timeout 200000.
    let skill_text = fs::read_to_string(&skill).unwrap();
    assert!(skill_text.contains("gcop-rs-managed:"), "missing sentinel");
    assert!(
        skill_text.contains("gcop-rs commit --split -y"),
        "skill missing canonical --split -y command"
    );
    assert!(
        skill_text.contains("200000"),
        "skill missing 200000ms timeout"
    );

    let block_text = fs::read_to_string(&claude_md).unwrap();
    assert!(block_text.contains("<!-- gcop-rs:begin"));
    assert!(block_text.contains("<!-- gcop-rs:end -->"));
    assert!(block_text.contains("gcop-rs commit --split -y"));
    assert!(block_text.contains("200000"));
}

#[test]
#[serial]
fn install_codex_creates_both_skill_and_block() {
    let tmp = TempDir::new().unwrap();
    let out = run(&["agent", "install", "codex"], None, Some(tmp.path()));
    assert_success(&out, "install codex");

    let skill = tmp.path().join("skills").join("gcop").join("SKILL.md");
    let agents_md = tmp.path().join("AGENTS.md");
    assert!(skill.exists());
    assert!(agents_md.exists());

    let skill_text = fs::read_to_string(&skill).unwrap();
    assert!(skill_text.contains("gcop-rs-managed:"));
    // Codex frontmatter uses metadata.short-description, not allowed-tools.
    assert!(skill_text.contains("metadata:"));
    assert!(skill_text.contains("gcop-rs commit --split -y"));
    assert!(skill_text.contains("200000"));

    let block_text = fs::read_to_string(&agents_md).unwrap();
    assert!(block_text.contains("<!-- gcop-rs:begin"));
    assert!(block_text.contains("gcop-rs commit --split -y"));
    assert!(block_text.contains("200000"));
}

#[test]
#[serial]
fn install_check_mode_creates_nothing() {
    let tmp = TempDir::new().unwrap();
    let out = run(
        &["agent", "install", "claude", "--check"],
        Some(tmp.path()),
        None,
    );
    assert_success(&out, "install --check");
    assert!(
        !tmp.path()
            .join("skills")
            .join("gcop")
            .join("SKILL.md")
            .exists()
    );
    assert!(!tmp.path().join("CLAUDE.md").exists());
    // stdout should mention "would create"
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("would create"),
        "expected dry-run hint in stdout, got: {}",
        stdout
    );
}

#[test]
#[serial]
fn install_idempotent_on_second_run() {
    let tmp = TempDir::new().unwrap();
    let first = run(&["agent", "install", "claude"], Some(tmp.path()), None);
    assert_success(&first, "first install");
    let second = run(&["agent", "install", "claude"], Some(tmp.path()), None);
    assert_success(&second, "second install");

    let stdout2 = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout2.contains("up-to-date"),
        "second run should report up-to-date; got: {}",
        stdout2
    );
}

#[test]
#[serial]
fn install_skill_only_skips_block() {
    let tmp = TempDir::new().unwrap();
    let out = run(
        &["agent", "install", "claude", "--skill-only"],
        Some(tmp.path()),
        None,
    );
    assert_success(&out, "install --skill-only");
    assert!(
        tmp.path()
            .join("skills")
            .join("gcop")
            .join("SKILL.md")
            .exists()
    );
    assert!(
        !tmp.path().join("CLAUDE.md").exists(),
        "block should be skipped"
    );
}

#[test]
#[serial]
fn install_instructions_only_skips_skill() {
    let tmp = TempDir::new().unwrap();
    let out = run(
        &["agent", "install", "claude", "--instructions-only"],
        Some(tmp.path()),
        None,
    );
    assert_success(&out, "install --instructions-only");
    assert!(
        !tmp.path()
            .join("skills")
            .join("gcop")
            .join("SKILL.md")
            .exists()
    );
    assert!(tmp.path().join("CLAUDE.md").exists());
}

#[test]
#[serial]
fn install_preserves_existing_claude_md_content() {
    let tmp = TempDir::new().unwrap();
    let claude_md = tmp.path().join("CLAUDE.md");
    let user_content = "# My personal instructions\n\nDo not erase me!\n";
    fs::write(&claude_md, user_content).unwrap();

    let out = run(&["agent", "install", "claude"], Some(tmp.path()), None);
    assert_success(&out, "install over user CLAUDE.md");

    let merged = fs::read_to_string(&claude_md).unwrap();
    assert!(
        merged.starts_with(user_content),
        "user content must be preserved at the top"
    );
    assert!(
        merged.contains("<!-- gcop-rs:begin"),
        "gcop block must be appended"
    );
}

#[test]
#[serial]
fn install_corrupted_claude_md_returns_nonzero_and_leaves_file_intact() {
    let tmp = TempDir::new().unwrap();
    let claude_md = tmp.path().join("CLAUDE.md");
    // BEGIN marker without END → corrupted state.
    let corrupted = "# header\n<!-- gcop-rs:begin v0.1.0 -->\noh no no end\n";
    fs::write(&claude_md, corrupted).unwrap();

    let out = run(&["agent", "install", "claude"], Some(tmp.path()), None);
    // Reporter prints an error message but the process exits 0 (we
    // report Corrupted in-band rather than failing the run). Either way
    // the file MUST be untouched.
    let after = fs::read_to_string(&claude_md).unwrap();
    assert_eq!(after, corrupted, "corrupted file must not be modified");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("corrupt"),
        "expected 'corrupted' in stderr; got: {}",
        stderr
    );
}

#[test]
#[serial]
fn install_foreign_skill_requires_force() {
    let tmp = TempDir::new().unwrap();
    let skill = tmp.path().join("skills").join("gcop").join("SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    let foreign = "---\nname: somebody-elses-skill\n---\nuser body\n";
    fs::write(&skill, foreign).unwrap();

    // Without --force, conflict.
    let out = run(
        &["agent", "install", "claude", "--skill-only"],
        Some(tmp.path()),
        None,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--force"),
        "conflict reason should mention --force; got: {}",
        stderr
    );
    let after = fs::read_to_string(&skill).unwrap();
    assert_eq!(after, foreign, "without --force, file must not be modified");

    // With --force, replaced.
    let out2 = run(
        &["agent", "install", "claude", "--skill-only", "--force"],
        Some(tmp.path()),
        None,
    );
    assert_success(&out2, "install --force");
    let after2 = fs::read_to_string(&skill).unwrap();
    assert!(after2.contains("gcop-rs-managed:"));
    assert!(!after2.contains("somebody-elses-skill"));
}

#[test]
#[serial]
fn uninstall_removes_both_artefacts_and_preserves_user_content() {
    let tmp = TempDir::new().unwrap();
    let claude_md = tmp.path().join("CLAUDE.md");
    let user_content = "# My personal notes\n";
    fs::write(&claude_md, user_content).unwrap();

    let install_out = run(&["agent", "install", "claude"], Some(tmp.path()), None);
    assert_success(&install_out, "install");

    let uninstall_out = run(&["agent", "uninstall", "claude"], Some(tmp.path()), None);
    assert_success(&uninstall_out, "uninstall");

    let skill = tmp.path().join("skills").join("gcop").join("SKILL.md");
    assert!(!skill.exists(), "skill must be removed");

    // CLAUDE.md should still exist and contain the user's original lines,
    // but without the gcop block.
    let after = fs::read_to_string(&claude_md).unwrap();
    assert!(
        after.contains("My personal notes"),
        "user content must be preserved on uninstall; got: {}",
        after
    );
    assert!(
        !after.contains("<!-- gcop-rs:begin"),
        "gcop block must be removed; got: {}",
        after
    );
}

#[test]
#[serial]
fn install_all_writes_both_agents() {
    let tmp_claude = TempDir::new().unwrap();
    let tmp_codex = TempDir::new().unwrap();
    let out = run(
        &["agent", "install", "all"],
        Some(tmp_claude.path()),
        Some(tmp_codex.path()),
    );
    assert_success(&out, "install all");
    assert!(
        tmp_claude
            .path()
            .join("skills")
            .join("gcop")
            .join("SKILL.md")
            .exists()
    );
    assert!(tmp_claude.path().join("CLAUDE.md").exists());
    assert!(
        tmp_codex
            .path()
            .join("skills")
            .join("gcop")
            .join("SKILL.md")
            .exists()
    );
    assert!(tmp_codex.path().join("AGENTS.md").exists());
}

#[test]
#[serial]
fn status_reports_not_installed_on_clean_dirs() {
    let tmp_claude = TempDir::new().unwrap();
    let tmp_codex = TempDir::new().unwrap();
    let out = run(
        &["agent", "status"],
        Some(tmp_claude.path()),
        Some(tmp_codex.path()),
    );
    assert_success(&out, "status clean");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should mention both agents and "not installed".
    assert!(stdout.contains("Claude Code"));
    assert!(stdout.contains("Codex"));
    assert!(stdout.contains("not installed"));
}

#[test]
#[serial]
fn status_reports_managed_after_install() {
    let tmp = TempDir::new().unwrap();
    let install_out = run(&["agent", "install", "claude"], Some(tmp.path()), None);
    assert_success(&install_out, "install");
    let status_out = run(&["agent", "status"], Some(tmp.path()), None);
    assert_success(&status_out, "status after install");
    let stdout = String::from_utf8_lossy(&status_out.stdout);
    assert!(stdout.contains("installed"), "stdout: {}", stdout);
    // Should print the current crate version in the report.
    let cur_version = format!("v{}", env!("CARGO_PKG_VERSION"));
    assert!(
        stdout.contains(&cur_version),
        "status should show current version {}; got: {}",
        cur_version,
        stdout
    );
}
