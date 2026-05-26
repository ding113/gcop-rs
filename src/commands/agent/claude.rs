//! Claude Code integration: SKILL.md + CLAUDE.md sentinel block.
//!
//! Composes the agent-neutral primitives ([`paths`], [`render`],
//! [`skill_writer`], [`instructions_writer`]) into Claude-specific
//! install / uninstall / status entry points.
//!
//! See [`crate::commands::agent::codex`] for the symmetric Codex module —
//! the duplication is intentional and acceptable while we only support
//! two agents (architecture note in `mod.rs`).
//!
//! [`paths`]: super::paths
//! [`render`]: super::render
//! [`skill_writer`]: super::skill_writer
//! [`instructions_writer`]: super::instructions_writer

use std::path::PathBuf;

use crate::commands::agent::instructions_writer::{
    BlockInstallReport, BlockUninstallReport, install_block, uninstall_block,
};
use crate::commands::agent::paths::{resolve_claude_md, resolve_claude_skill_dir};
use crate::commands::agent::render::{render_block, render_skill, templates};
use crate::commands::agent::skill_writer::{
    InstallReport, UninstallReport, install_skill, uninstall_skill,
};
use crate::commands::agent::{FileState, block_file_state, skill_file_state};
use crate::error::Result;

/// What gcop-rs version is bundled into the running binary. Used as the
/// sentinel version for fresh installs and as the comparison target for
/// idempotency.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the absolute path where the SKILL.md will be written.
pub fn skill_path() -> Result<PathBuf> {
    Ok(resolve_claude_skill_dir()?.join("SKILL.md"))
}

/// Returns the absolute path of the CLAUDE.md that hosts the sentinel block.
pub fn instructions_path() -> Result<PathBuf> {
    resolve_claude_md()
}

/// Install both artefacts (SKILL.md + CLAUDE.md block) unless the caller
/// requests a partial install via `skill_only` / `instructions_only`.
///
/// `check == true` performs no IO; reports indicate "would do X".
///
/// `force` only applies to the SKILL.md half — overwriting a foreign
/// SKILL.md. The CLAUDE.md block is always safe to install (append on
/// missing, replace on version mismatch, refuse on corruption), so no
/// `--force` is needed there.
pub fn install(
    force: bool,
    check: bool,
    skill_only: bool,
    instructions_only: bool,
) -> Result<ClaudeInstallReport> {
    let skill = if instructions_only {
        None
    } else {
        let target = skill_path()?;
        let rendered = render_skill(templates::SKILL_CLAUDE, CURRENT_VERSION)?;
        Some(install_skill(
            &target,
            &rendered,
            CURRENT_VERSION,
            force,
            check,
        )?)
    };

    let block = if skill_only {
        None
    } else {
        let target = instructions_path()?;
        let rendered = render_block(templates::INSTRUCTIONS_BLOCK, CURRENT_VERSION)?;
        Some(install_block(&target, &rendered, CURRENT_VERSION, check)?)
    };

    Ok(ClaudeInstallReport { skill, block })
}

/// Remove both artefacts. Either half being already absent / foreign is
/// a successful no-op for that half.
pub fn uninstall() -> Result<ClaudeUninstallReport> {
    let skill = uninstall_skill(&skill_path()?)?;
    let block = uninstall_block(&instructions_path()?)?;
    Ok(ClaudeUninstallReport { skill, block })
}

/// Report the on-disk state of both artefacts (read-only).
pub fn status() -> Result<ClaudeStatus> {
    let skill_path = skill_path()?;
    let instructions_path = instructions_path()?;
    let skill = skill_file_state(&skill_path)?;
    let block = block_file_state(&instructions_path)?;
    Ok(ClaudeStatus {
        skill_path,
        instructions_path,
        skill,
        block,
    })
}

/// Composite result of [`install`]: each half is `None` when that half was
/// skipped via `--skill-only` / `--instructions-only`.
#[derive(Debug)]
pub struct ClaudeInstallReport {
    pub skill: Option<InstallReport>,
    pub block: Option<BlockInstallReport>,
}

/// Composite result of [`uninstall`]: both halves are always present.
#[derive(Debug)]
pub struct ClaudeUninstallReport {
    pub skill: UninstallReport,
    pub block: BlockUninstallReport,
}

/// Composite result of [`status`].
#[derive(Debug)]
pub struct ClaudeStatus {
    pub skill_path: PathBuf,
    pub instructions_path: PathBuf,
    pub skill: FileState,
    pub block: FileState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::instructions_writer::BlockInstallReportKind;
    use crate::commands::agent::skill_writer::{InstallReportKind, UninstallReport as URpt};
    use serial_test::serial;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// Set GCOP_CLAUDE_DIR for the duration of one test, restoring on drop.
    struct ClaudeDirGuard {
        prev: Option<OsString>,
    }

    impl ClaudeDirGuard {
        fn set(dir: &Path) -> Self {
            let prev = std::env::var_os("GCOP_CLAUDE_DIR");
            // SAFETY: tests using this guard are #[serial].
            unsafe { std::env::set_var("GCOP_CLAUDE_DIR", dir) };
            Self { prev }
        }
    }

    impl Drop for ClaudeDirGuard {
        fn drop(&mut self) {
            // SAFETY: see Self::set.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var("GCOP_CLAUDE_DIR", v),
                    None => std::env::remove_var("GCOP_CLAUDE_DIR"),
                }
            }
        }
    }

    #[test]
    #[serial]
    fn install_writes_both_skill_and_block_by_default() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());

        let report = install(false, false, false, false).unwrap();
        let skill = report.skill.expect("skill half should run by default");
        let block = report.block.expect("block half should run by default");

        assert_eq!(skill.kind, InstallReportKind::Created);
        assert_eq!(block.kind, BlockInstallReportKind::Created);

        // Files exist with the expected sentinels.
        let skill_md = fs::read_to_string(skill_path().unwrap()).unwrap();
        assert!(skill_md.contains("gcop-rs-managed:"));
        let claude_md = fs::read_to_string(instructions_path().unwrap()).unwrap();
        assert!(claude_md.contains("<!-- gcop-rs:begin"));
    }

    #[test]
    #[serial]
    fn skill_only_skips_block_half() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());

        let report = install(false, false, true, false).unwrap();
        assert!(report.skill.is_some());
        assert!(report.block.is_none());
        assert!(skill_path().unwrap().exists());
        assert!(!instructions_path().unwrap().exists());
    }

    #[test]
    #[serial]
    fn instructions_only_skips_skill_half() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());

        let report = install(false, false, false, true).unwrap();
        assert!(report.skill.is_none());
        assert!(report.block.is_some());
        assert!(!skill_path().unwrap().exists());
        assert!(instructions_path().unwrap().exists());
    }

    #[test]
    #[serial]
    fn check_mode_does_not_touch_disk() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());

        let report = install(false, true, false, false).unwrap();
        assert_eq!(report.skill.unwrap().kind, InstallReportKind::WouldCreate);
        assert_eq!(
            report.block.unwrap().kind,
            BlockInstallReportKind::WouldCreate
        );
        assert!(!skill_path().unwrap().exists());
        assert!(!instructions_path().unwrap().exists());
    }

    #[test]
    #[serial]
    fn install_then_uninstall_round_trips() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());

        install(false, false, false, false).unwrap();
        let report = uninstall().unwrap();
        assert_eq!(report.skill, URpt::Removed);
        // CLAUDE.md contained only the block → file is now gone.
        match report.block {
            BlockUninstallReport::RemovedAndDeletedEmptyFile => {}
            other => panic!("expected RemovedAndDeletedEmptyFile, got {:?}", other),
        }
        assert!(!skill_path().unwrap().exists());
        assert!(!instructions_path().unwrap().exists());
    }

    #[test]
    #[serial]
    fn status_reports_not_installed_on_clean_dir() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());
        let s = status().unwrap();
        assert_eq!(s.skill, FileState::NotInstalled);
        assert_eq!(s.block, FileState::NotInstalled);
    }

    #[test]
    #[serial]
    fn status_reports_managed_after_install() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());
        install(false, false, false, false).unwrap();
        let s = status().unwrap();
        match s.skill {
            FileState::InstalledManaged { version } => {
                assert_eq!(version.as_deref(), Some(&*format!("v{}", CURRENT_VERSION)));
            }
            other => panic!("expected InstalledManaged for skill, got {:?}", other),
        }
        match s.block {
            FileState::InstalledManaged { version } => {
                assert_eq!(version.as_deref(), Some(&*format!("v{}", CURRENT_VERSION)));
            }
            other => panic!("expected InstalledManaged for block, got {:?}", other),
        }
    }

    #[test]
    #[serial]
    fn install_preserves_existing_claude_md_user_content() {
        let tmp = TempDir::new().unwrap();
        let _g = ClaudeDirGuard::set(tmp.path());
        // Pre-seed user CLAUDE.md.
        let claude_md = instructions_path().unwrap();
        fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
        fs::write(&claude_md, "# My personal notes\n\nDo not erase.\n").unwrap();

        install(false, false, false, false).unwrap();

        let on_disk = fs::read_to_string(&claude_md).unwrap();
        assert!(on_disk.contains("My personal notes"));
        assert!(on_disk.contains("Do not erase."));
        assert!(on_disk.contains("<!-- gcop-rs:begin"));
    }
}
