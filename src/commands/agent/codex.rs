//! Codex integration: SKILL.md + AGENTS.md sentinel block.
//!
//! Structural twin of [`crate::commands::agent::claude`]. The only diffs
//! are which paths and which SKILL template we feed into the agent-neutral
//! primitives. We accept the duplication for now (≤2 agents); the day a
//! third agent lands, refactor both into a `trait AgentInstaller`.

use std::path::PathBuf;

use crate::commands::agent::instructions_writer::{
    BlockInstallReport, BlockUninstallReport, install_block, uninstall_block,
};
use crate::commands::agent::paths::{resolve_codex_agents_md, resolve_codex_skill_dir};
use crate::commands::agent::render::{render_block, render_skill, templates};
use crate::commands::agent::skill_writer::{
    InstallReport, UninstallReport, install_skill, uninstall_skill,
};
use crate::commands::agent::{FileState, block_file_state, skill_file_state};
use crate::error::Result;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn skill_path() -> Result<PathBuf> {
    Ok(resolve_codex_skill_dir()?.join("SKILL.md"))
}

pub fn instructions_path() -> Result<PathBuf> {
    resolve_codex_agents_md()
}

pub fn install(
    force: bool,
    check: bool,
    skill_only: bool,
    instructions_only: bool,
) -> Result<CodexInstallReport> {
    let skill = if instructions_only {
        None
    } else {
        let target = skill_path()?;
        let rendered = render_skill(templates::SKILL_CODEX, CURRENT_VERSION)?;
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

    Ok(CodexInstallReport { skill, block })
}

pub fn uninstall() -> Result<CodexUninstallReport> {
    let skill = uninstall_skill(&skill_path()?)?;
    let block = uninstall_block(&instructions_path()?)?;
    Ok(CodexUninstallReport { skill, block })
}

pub fn status() -> Result<CodexStatus> {
    let skill_path = skill_path()?;
    let instructions_path = instructions_path()?;
    let skill = skill_file_state(&skill_path)?;
    let block = block_file_state(&instructions_path)?;
    Ok(CodexStatus {
        skill_path,
        instructions_path,
        skill,
        block,
    })
}

#[derive(Debug)]
pub struct CodexInstallReport {
    pub skill: Option<InstallReport>,
    pub block: Option<BlockInstallReport>,
}

#[derive(Debug)]
pub struct CodexUninstallReport {
    pub skill: UninstallReport,
    pub block: BlockUninstallReport,
}

#[derive(Debug)]
pub struct CodexStatus {
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

    struct CodexDirGuard {
        prev: Option<OsString>,
    }

    impl CodexDirGuard {
        fn set(dir: &Path) -> Self {
            let prev = std::env::var_os("CODEX_HOME");
            // SAFETY: tests using this guard are #[serial].
            unsafe { std::env::set_var("CODEX_HOME", dir) };
            Self { prev }
        }
    }

    impl Drop for CodexDirGuard {
        fn drop(&mut self) {
            // SAFETY: see Self::set.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var("CODEX_HOME", v),
                    None => std::env::remove_var("CODEX_HOME"),
                }
            }
        }
    }

    #[test]
    #[serial]
    fn install_writes_skill_and_block_by_default() {
        let tmp = TempDir::new().unwrap();
        let _g = CodexDirGuard::set(tmp.path());

        let report = install(false, false, false, false).unwrap();
        assert_eq!(report.skill.unwrap().kind, InstallReportKind::Created);
        assert_eq!(report.block.unwrap().kind, BlockInstallReportKind::Created);

        let agents_md = fs::read_to_string(instructions_path().unwrap()).unwrap();
        assert!(agents_md.contains("<!-- gcop-rs:begin"));
        let skill_md = fs::read_to_string(skill_path().unwrap()).unwrap();
        assert!(skill_md.contains("gcop-rs-managed:"));
        // Codex template uses metadata.short-description, not allowed-tools.
        assert!(skill_md.contains("metadata:"));
    }

    #[test]
    #[serial]
    fn skill_only_skips_block_half() {
        let tmp = TempDir::new().unwrap();
        let _g = CodexDirGuard::set(tmp.path());
        let report = install(false, false, true, false).unwrap();
        assert!(report.skill.is_some());
        assert!(report.block.is_none());
        assert!(!instructions_path().unwrap().exists());
    }

    #[test]
    #[serial]
    fn instructions_only_skips_skill_half() {
        let tmp = TempDir::new().unwrap();
        let _g = CodexDirGuard::set(tmp.path());
        let report = install(false, false, false, true).unwrap();
        assert!(report.skill.is_none());
        assert!(report.block.is_some());
        assert!(!skill_path().unwrap().exists());
    }

    #[test]
    #[serial]
    fn check_mode_does_not_write() {
        let tmp = TempDir::new().unwrap();
        let _g = CodexDirGuard::set(tmp.path());
        install(false, true, false, false).unwrap();
        assert!(!skill_path().unwrap().exists());
        assert!(!instructions_path().unwrap().exists());
    }

    #[test]
    #[serial]
    fn install_then_uninstall_round_trips() {
        let tmp = TempDir::new().unwrap();
        let _g = CodexDirGuard::set(tmp.path());
        install(false, false, false, false).unwrap();
        let report = uninstall().unwrap();
        assert_eq!(report.skill, URpt::Removed);
        match report.block {
            BlockUninstallReport::RemovedAndDeletedEmptyFile => {}
            other => panic!("expected RemovedAndDeletedEmptyFile, got {:?}", other),
        }
    }

    #[test]
    #[serial]
    fn status_clean_dir_not_installed() {
        let tmp = TempDir::new().unwrap();
        let _g = CodexDirGuard::set(tmp.path());
        let s = status().unwrap();
        assert_eq!(s.skill, FileState::NotInstalled);
        assert_eq!(s.block, FileState::NotInstalled);
    }

    #[test]
    #[serial]
    fn install_preserves_existing_agents_md() {
        let tmp = TempDir::new().unwrap();
        let _g = CodexDirGuard::set(tmp.path());
        let agents_md = instructions_path().unwrap();
        fs::create_dir_all(agents_md.parent().unwrap()).unwrap();
        fs::write(&agents_md, "# Codex AGENTS.md\n\nKeep this.\n").unwrap();

        install(false, false, false, false).unwrap();

        let on_disk = fs::read_to_string(&agents_md).unwrap();
        assert!(on_disk.contains("Codex AGENTS.md"));
        assert!(on_disk.contains("Keep this."));
        assert!(on_disk.contains("<!-- gcop-rs:begin"));
    }
}
