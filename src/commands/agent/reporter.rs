//! CLI-side reporter for the `agent` subcommand.
//!
//! Bridges the structured `*Report` enums from `claude` / `codex` into the
//! gcop-rs colored output style. `main.rs` only depends on this module —
//! the underlying writers stay free of stdout/stderr coupling so they
//! remain trivially testable.
//!
//! Each function:
//!
//! 1. Prints a `[Agent name] step ...` header.
//! 2. Delegates to the underlying `claude::install` / `codex::install` etc.
//! 3. Pretty-prints the resulting report, line by line, through `ui::*`.
//! 4. Surfaces a final `warning` if the `gcop-rs` binary is not on `PATH`
//!    (since an installed skill is useless without the binary).

use crate::commands::agent::FileState;
use crate::commands::agent::claude;
use crate::commands::agent::codex;
use crate::commands::agent::instructions_writer::{
    BlockInstallReport, BlockInstallReportKind, BlockUninstallReport,
};
use crate::commands::agent::skill_writer::{InstallReport, InstallReportKind, UninstallReport};
use crate::error::Result;
use crate::ui::colors as ui;
use std::path::Path;

const CLAUDE_LABEL: &str = "Claude Code";
const CODEX_LABEL: &str = "Codex";

pub fn install_claude(
    force: bool,
    check: bool,
    skill_only: bool,
    instructions_only: bool,
    colored: bool,
) -> Result<()> {
    ui::step(CLAUDE_LABEL, "installing gcop-rs integration", colored);
    let report = claude::install(force, check, skill_only, instructions_only)?;
    print_install_report(&report.skill, &report.block, check, colored);
    check_path_warning(colored);
    Ok(())
}

pub fn install_codex(
    force: bool,
    check: bool,
    skill_only: bool,
    instructions_only: bool,
    colored: bool,
) -> Result<()> {
    ui::step(CODEX_LABEL, "installing gcop-rs integration", colored);
    let report = codex::install(force, check, skill_only, instructions_only)?;
    print_install_report(&report.skill, &report.block, check, colored);
    check_path_warning(colored);
    Ok(())
}

pub fn uninstall_claude(colored: bool) -> Result<()> {
    ui::step(CLAUDE_LABEL, "uninstalling gcop-rs integration", colored);
    let report = claude::uninstall()?;
    print_uninstall_report(&report.skill, &report.block, colored);
    Ok(())
}

pub fn uninstall_codex(colored: bool) -> Result<()> {
    ui::step(CODEX_LABEL, "uninstalling gcop-rs integration", colored);
    let report = codex::uninstall()?;
    print_uninstall_report(&report.skill, &report.block, colored);
    Ok(())
}

pub fn status_claude(colored: bool) -> Result<()> {
    let st = claude::status()?;
    print_status(
        CLAUDE_LABEL,
        &st.skill_path,
        &st.instructions_path,
        &st.skill,
        &st.block,
        colored,
    );
    Ok(())
}

pub fn status_codex(colored: bool) -> Result<()> {
    let st = codex::status()?;
    print_status(
        CODEX_LABEL,
        &st.skill_path,
        &st.instructions_path,
        &st.skill,
        &st.block,
        colored,
    );
    Ok(())
}

// ---------- Pretty-printers ----------

fn print_install_report(
    skill: &Option<InstallReport>,
    block: &Option<BlockInstallReport>,
    check: bool,
    colored: bool,
) {
    if let Some(s) = skill {
        match &s.kind {
            InstallReportKind::Created => ui::success(
                &format!("Installed SKILL.md at {}", s.path.display()),
                colored,
            ),
            InstallReportKind::UpToDate => println!(
                "{}",
                ui::info(
                    &format!("SKILL.md already up-to-date at {}", s.path.display()),
                    colored,
                )
            ),
            InstallReportKind::Upgraded {
                from_version,
                to_version,
            } => ui::success(
                &format!(
                    "Upgraded SKILL.md {} → {} at {}",
                    from_version.as_deref().unwrap_or("(unknown)"),
                    to_version,
                    s.path.display(),
                ),
                colored,
            ),
            InstallReportKind::Replaced => ui::warning(
                &format!("Force-replaced foreign SKILL.md at {}", s.path.display()),
                colored,
            ),
            InstallReportKind::Conflict { reason } => ui::error(
                &format!(
                    "Foreign SKILL.md at {} → {} (pass --force to overwrite)",
                    s.path.display(),
                    reason
                ),
                colored,
            ),
            InstallReportKind::WouldCreate => ui::warning(
                &format!("[check] would create SKILL.md at {}", s.path.display()),
                colored,
            ),
            InstallReportKind::WouldUpgrade {
                from_version,
                to_version,
            } => ui::warning(
                &format!(
                    "[check] would upgrade SKILL.md {} → {} at {}",
                    from_version.as_deref().unwrap_or("(unknown)"),
                    to_version,
                    s.path.display(),
                ),
                colored,
            ),
            InstallReportKind::WouldReplace => ui::warning(
                &format!(
                    "[check] would force-replace SKILL.md at {}",
                    s.path.display()
                ),
                colored,
            ),
        }
    }
    if let Some(b) = block {
        match &b.kind {
            BlockInstallReportKind::Created => ui::success(
                &format!("Wrote new {} with gcop block", b.path.display()),
                colored,
            ),
            BlockInstallReportKind::Appended => ui::success(
                &format!("Appended gcop block to {}", b.path.display()),
                colored,
            ),
            BlockInstallReportKind::UpToDate => println!(
                "{}",
                ui::info(
                    &format!("gcop block already up-to-date in {}", b.path.display()),
                    colored,
                )
            ),
            BlockInstallReportKind::Replaced {
                from_version,
                to_version,
            } => ui::success(
                &format!(
                    "Upgraded gcop block {} → {} in {}",
                    from_version.as_deref().unwrap_or("(unknown)"),
                    to_version,
                    b.path.display(),
                ),
                colored,
            ),
            BlockInstallReportKind::Corrupted { reason } => ui::error(
                &format!(
                    "Refused to modify corrupted {}: {}",
                    b.path.display(),
                    reason
                ),
                colored,
            ),
            BlockInstallReportKind::WouldCreate => ui::warning(
                &format!("[check] would create {} with gcop block", b.path.display()),
                colored,
            ),
            BlockInstallReportKind::WouldAppend => ui::warning(
                &format!("[check] would append gcop block to {}", b.path.display()),
                colored,
            ),
            BlockInstallReportKind::WouldReplace {
                from_version,
                to_version,
            } => ui::warning(
                &format!(
                    "[check] would upgrade gcop block {} → {} in {}",
                    from_version.as_deref().unwrap_or("(unknown)"),
                    to_version,
                    b.path.display(),
                ),
                colored,
            ),
        }
    }
    let _ = check; // currently informational only
}

fn print_uninstall_report(skill: &UninstallReport, block: &BlockUninstallReport, colored: bool) {
    match skill {
        UninstallReport::Removed => ui::success("Removed gcop SKILL.md", colored),
        UninstallReport::SkippedNotFound => println!(
            "{}",
            ui::info("SKILL.md not found; nothing to remove", colored)
        ),
        UninstallReport::SkippedForeign => ui::warning(
            "SKILL.md exists but is not managed by gcop-rs; left alone",
            colored,
        ),
    }
    match block {
        BlockUninstallReport::Removed => {
            ui::success("Removed gcop block (preserved user content)", colored);
        }
        BlockUninstallReport::RemovedAndDeletedEmptyFile => {
            ui::success("Removed gcop block; file was empty so deleted", colored);
        }
        BlockUninstallReport::SkippedNotFound => println!(
            "{}",
            ui::info("Instructions file not found; nothing to remove", colored,)
        ),
        BlockUninstallReport::SkippedNoBlock => println!(
            "{}",
            ui::info(
                "Instructions file has no gcop block; nothing to remove",
                colored,
            )
        ),
        BlockUninstallReport::Corrupted { reason } => ui::error(
            &format!("Corrupted block; refused to modify: {}", reason),
            colored,
        ),
    }
}

fn print_status(
    label: &str,
    skill_path: &Path,
    block_path: &Path,
    skill: &FileState,
    block: &FileState,
    colored: bool,
) {
    ui::step(label, "status", colored);
    print_file_state("skill", skill_path, skill, colored);
    print_file_state("block", block_path, block, colored);
}

fn print_file_state(kind: &str, path: &Path, state: &FileState, colored: bool) {
    match state {
        FileState::InstalledManaged { version } => ui::success(
            &format!(
                "{}: installed {} at {}",
                kind,
                version.as_deref().unwrap_or("(unknown version)"),
                path.display(),
            ),
            colored,
        ),
        FileState::Foreign => ui::warning(
            &format!(
                "{}: foreign file at {} (not managed by gcop)",
                kind,
                path.display(),
            ),
            colored,
        ),
        FileState::NotInstalled => println!(
            "{}",
            ui::info(
                &format!("{}: not installed (target: {})", kind, path.display()),
                colored,
            )
        ),
    }
}

fn check_path_warning(colored: bool) {
    if which::which("gcop-rs").is_err() {
        ui::warning(
            "`gcop-rs` binary not found on PATH; agents won't be able to invoke it. \
             Install it via `cargo install gcop-rs` or `cargo binstall gcop-rs`.",
            colored,
        );
    }
}
