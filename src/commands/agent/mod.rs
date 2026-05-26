//! Coding-agent integration commands.
//!
//! Exposes `gcop-rs agent install/uninstall/status` to write skill files and
//! always-on prompt blocks into the user's `~/.claude/` and `~/.codex/`
//! directories, so coding agents (Claude Code, Codex) discover gcop-rs and
//! prefer it for commit generation.
//!
//! # Module layout
//! - [`paths`] — cross-platform path resolution for `~/.claude` and `~/.codex`,
//!   with `GCOP_CLAUDE_DIR` / `CODEX_HOME` env overrides for testing.
//!
//! - [`sentinel`] — pure-function detectors for files that gcop-rs manages.
//!   Two formats: YAML key in SKILL.md frontmatter (Step 3) and HTML comment
//!   block in CLAUDE.md / AGENTS.md (Step 4).
//! - [`render`] — pure renderers that inject the running gcop-rs version
//!   into bundled templates (Step 5). Also exposes the templates as compile-
//!   time constants via [`render::templates`] for higher layers.
//! - [`skill_writer`] — decision planner ([`skill_writer::plan_skill_action`])
//!   today (Step 6); IO orchestration (`install_skill` / `uninstall_skill`)
//!   added in Step 8.
//! - [`atomic_io`] — atomic file writes (`tempfile` + rename) and parent-dir
//!   creation; the only module in `agent/` that touches the filesystem
//!   directly. Higher-level writers compose on top of it (Step 7).
//! - [`instructions_writer`] — installs / replaces / removes the gcop-rs
//!   sentinel block inside CLAUDE.md and AGENTS.md, preserving the rest of
//!   the user's content byte-for-byte (Step 9).
//! - [`claude`] / [`codex`] — agent-specific thin wrappers that compose
//!   `paths` + `render::templates` + `skill_writer` + `instructions_writer`
//!   into a single install/uninstall/status surface (Step 10).
//! - [`reporter`] — CLI-side I/O: takes the structured `*Report` types
//!   produced by `claude` / `codex` and prints them through `crate::ui`
//!   in the gcop-rs colored-output style. This is the only module in
//!   `agent/` that writes to stdout/stderr (Step 11).
//!
//! # Status types
//!
//! [`FileState`] is the shared status enum returned by both agent modules.
//! Two helpers report the on-disk state of the SKILL.md and the CLAUDE.md /
//! AGENTS.md block respectively, without modifying anything.

pub mod atomic_io;
pub mod claude;
pub mod codex;
pub mod instructions_writer;
pub mod paths;
pub mod render;
pub mod reporter;
pub mod sentinel;
pub mod skill_writer;

use std::fs;
use std::io;
use std::path::Path;

use crate::commands::agent::sentinel::{
    BLOCK_BEGIN_PREFIX, BLOCK_END, is_skill_gcop_managed, skill_managed_version,
};
use crate::error::Result;

/// Whether a target file is missing, gcop-managed, or owned by the user.
///
/// Returned by [`skill_file_state`] and [`block_file_state`]. Read-only:
/// computing the state never touches disk except for the read.
#[derive(Debug, PartialEq, Eq)]
pub enum FileState {
    /// File does not exist on disk.
    NotInstalled,
    /// gcop-managed (carries the recorded version, if extractable).
    InstalledManaged { version: Option<String> },
    /// File exists but is NOT gcop-managed; treat as user content.
    Foreign,
}

/// Report the state of a SKILL.md at `target`.
pub fn skill_file_state(target: &Path) -> Result<FileState> {
    match fs::read_to_string(target) {
        Ok(s) if is_skill_gcop_managed(&s) => Ok(FileState::InstalledManaged {
            version: skill_managed_version(&s),
        }),
        Ok(_) => Ok(FileState::Foreign),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FileState::NotInstalled),
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("failed to read {}: {}", target.display(), e),
        )
        .into()),
    }
}

/// Report whether `target` (CLAUDE.md or AGENTS.md) contains a gcop-rs block.
///
/// Returns:
/// - `NotInstalled` if the file does not exist.
/// - `InstalledManaged` if a well-formed gcop block is present (carries
///   the version it declares).
/// - `Foreign` if the file exists with no gcop block (entirely user content).
pub fn block_file_state(target: &Path) -> Result<FileState> {
    let content = match fs::read_to_string(target) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(FileState::NotInstalled);
        }
        Err(e) => {
            return Err(io::Error::new(
                e.kind(),
                format!("failed to read {}: {}", target.display(), e),
            )
            .into());
        }
    };

    let begin = content.find(BLOCK_BEGIN_PREFIX);
    let end_start = content.find(BLOCK_END);

    match (begin, end_start) {
        (Some(b), Some(e)) if b < e => {
            let end_exclusive = e + BLOCK_END.len();
            let block = &content[b..end_exclusive];
            Ok(FileState::InstalledManaged {
                version: sentinel::extract_block_version(block),
            })
        }
        // Either missing markers entirely, or malformed — treat as no
        // gcop block. (Corrupted blocks show up loudly during install,
        // not status.)
        _ => Ok(FileState::Foreign),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::render::{render_block, render_skill, templates};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn skill_file_state_not_installed_when_missing() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("SKILL.md");
        assert_eq!(skill_file_state(&target).unwrap(), FileState::NotInstalled);
    }

    #[test]
    fn skill_file_state_managed_when_gcop_skill() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("SKILL.md");
        let content = render_skill(templates::SKILL_CLAUDE, "0.14.0").unwrap();
        fs::write(&target, &content).unwrap();
        match skill_file_state(&target).unwrap() {
            FileState::InstalledManaged { version } => {
                assert_eq!(version.as_deref(), Some("v0.14.0"));
            }
            other => panic!("expected InstalledManaged, got {:?}", other),
        }
    }

    #[test]
    fn skill_file_state_foreign_when_not_gcop() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("SKILL.md");
        fs::write(&target, "---\nname: somebody-else\n---\n").unwrap();
        assert_eq!(skill_file_state(&target).unwrap(), FileState::Foreign);
    }

    #[test]
    fn block_file_state_not_installed_when_missing() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("CLAUDE.md");
        assert_eq!(block_file_state(&target).unwrap(), FileState::NotInstalled);
    }

    #[test]
    fn block_file_state_managed_when_block_present() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("CLAUDE.md");
        let block = render_block(templates::INSTRUCTIONS_BLOCK, "0.14.0").unwrap();
        fs::write(&target, format!("user content\n\n{}", block)).unwrap();
        match block_file_state(&target).unwrap() {
            FileState::InstalledManaged { version } => {
                assert_eq!(version.as_deref(), Some("v0.14.0"));
            }
            other => panic!("expected InstalledManaged, got {:?}", other),
        }
    }

    #[test]
    fn block_file_state_foreign_when_no_block() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("CLAUDE.md");
        fs::write(&target, "# My CLAUDE.md\n\nNo gcop block.\n").unwrap();
        assert_eq!(block_file_state(&target).unwrap(), FileState::Foreign);
    }
}
