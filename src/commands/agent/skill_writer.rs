//! SKILL.md install/uninstall orchestration.
//!
//! Split into two responsibilities:
//!
//! 1. **Decision** ([`plan_skill_action`]): pure function. Given the
//!    existing file content (or `None`) and the rendered new content,
//!    return what action to take. No IO, fully testable.
//! 2. **IO** ([`install_skill`] / [`uninstall_skill`]): thin shell that
//!    reads the target file (if any), calls `plan_skill_action`, and
//!    performs the side effect (or doesn't, under `--check`).
//!
//! This separation lets us cover every branch of the decision table with
//! cheap unit tests, and lets the IO layer remain a small, predictable
//! orchestration of read → decide → write.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::commands::agent::atomic_io::atomic_write;
use crate::commands::agent::sentinel::{is_skill_gcop_managed, skill_managed_version};
use crate::error::Result;

/// Decision returned by [`plan_skill_action`] for IO callers to execute.
///
/// Each variant carries everything needed to perform the IO — the IO layer
/// does not re-inspect existing content.
#[derive(Debug, PartialEq, Eq)]
pub enum SkillAction {
    /// File does not exist on disk → write the new content.
    Create { content: String },

    /// File exists, is gcop-managed, and version matches → no-op.
    SkipUpToDate,

    /// File exists, is gcop-managed, and version differs → overwrite (upgrade).
    Replace { content: String },

    /// File exists but is NOT gcop-managed → refuse to overwrite without
    /// `--force`. `reason` is user-facing and explains how to proceed.
    RequireForce { reason: String },
}

/// Pure decision: given the on-disk content (or `None`), the freshly
/// rendered content (already contains the current sentinel), the running
/// gcop-rs version, and whether `--force` is set, return the action.
///
/// Decision table:
///
/// | existing                         | force | result          |
/// |----------------------------------|-------|-----------------|
/// | `None`                           | any   | `Create`        |
/// | gcop-managed, same version       | any   | `SkipUpToDate`  |
/// | gcop-managed, different version  | any   | `Replace`       |
/// | not gcop-managed                 | false | `RequireForce`  |
/// | not gcop-managed                 | true  | `Replace`       |
///
/// `current_version` is the version we're trying to install, expressed as
/// the bare semver (e.g. `"0.14.0"`). The sentinel inside the file stores
/// the version with a leading `v` (e.g. `"v0.14.0"`); this function
/// reconciles the two formats so callers don't need to.
pub fn plan_skill_action(
    existing: Option<&str>,
    rendered: &str,
    current_version: &str,
    force: bool,
) -> SkillAction {
    let content = match existing {
        None => {
            return SkillAction::Create {
                content: rendered.to_string(),
            };
        }
        Some(s) => s,
    };

    if is_skill_gcop_managed(content) {
        let existing_version = skill_managed_version(content);
        if versions_equal(existing_version.as_deref(), current_version) {
            SkillAction::SkipUpToDate
        } else {
            SkillAction::Replace {
                content: rendered.to_string(),
            }
        }
    } else if force {
        SkillAction::Replace {
            content: rendered.to_string(),
        }
    } else {
        SkillAction::RequireForce {
            reason: "existing SKILL.md is not managed by gcop-rs; pass `--force` to overwrite, \
                 or remove the file manually if you don't need its content"
                .to_string(),
        }
    }
}

/// True if `existing` matches `expected_version`, treating an optional
/// leading `v` as equivalent (so `"v0.14.0"` == `"0.14.0"`).
fn versions_equal(existing: Option<&str>, expected_version: &str) -> bool {
    let existing = match existing {
        Some(v) => v,
        None => return false,
    };
    let existing_bare = existing.strip_prefix('v').unwrap_or(existing);
    let expected_bare = expected_version
        .strip_prefix('v')
        .unwrap_or(expected_version);
    existing_bare == expected_bare
}

// =============================================================================
// IO orchestration: install_skill / uninstall_skill
// =============================================================================

/// What `install_skill` did — the high-level outcome, for callers to print
/// or assert against.
///
/// Distinguishing `Upgraded` vs `Replaced` vs `Created` lets the CLI layer
/// emit a different message for each: an upgrade is routine, a forced
/// foreign-file overwrite warrants a louder warning.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallReportKind {
    /// New file was created.
    Created,
    /// Already matches current version — no IO performed.
    UpToDate,
    /// Existing gcop-managed file was upgraded to the current version.
    Upgraded {
        from_version: Option<String>,
        to_version: String,
    },
    /// Foreign (non-gcop) file was overwritten because `--force` was set.
    Replaced,
    /// Foreign file detected; user must pass `--force` to overwrite.
    Conflict { reason: String },
    /// `--check` was set and we would have created the file.
    WouldCreate,
    /// `--check` was set and we would have upgraded the file.
    WouldUpgrade {
        from_version: Option<String>,
        to_version: String,
    },
    /// `--check` was set and we would have force-replaced a foreign file.
    WouldReplace,
}

/// Result of [`install_skill`]: where we wrote (or would write) and what happened.
#[derive(Debug)]
pub struct InstallReport {
    pub kind: InstallReportKind,
    pub path: PathBuf,
}

/// Result of [`uninstall_skill`].
#[derive(Debug, PartialEq, Eq)]
pub enum UninstallReport {
    /// gcop-managed SKILL.md was removed.
    Removed,
    /// Target file did not exist; nothing to do.
    SkippedNotFound,
    /// Target file exists but is NOT gcop-managed; we left it alone.
    SkippedForeign,
}

/// Install (or check) a gcop SKILL.md at `target`.
///
/// - `rendered`: the full file content to write (already rendered by
///   [`crate::commands::agent::render::render_skill`]).
/// - `current_version`: the version we're trying to install, bare semver
///   (e.g. `"0.14.0"`). Used by the decision layer to compare against any
///   existing sentinel.
/// - `force`: if `true`, overwrite a foreign (non-gcop) SKILL.md instead
///   of returning a `Conflict`.
/// - `check`: if `true`, perform NO IO; just compute what would happen.
///
/// Idempotent: running back-to-back with the same arguments yields
/// `UpToDate` after the first call.
pub fn install_skill(
    target: &Path,
    rendered: &str,
    current_version: &str,
    force: bool,
    check: bool,
) -> Result<InstallReport> {
    let existing = read_optional(target)?;
    let action = plan_skill_action(existing.as_deref(), rendered, current_version, force);

    let kind = match action {
        SkillAction::Create { content } => {
            if check {
                InstallReportKind::WouldCreate
            } else {
                atomic_write(target, &content)?;
                InstallReportKind::Created
            }
        }
        SkillAction::SkipUpToDate => InstallReportKind::UpToDate,
        SkillAction::Replace { content } => {
            let was_managed = existing
                .as_deref()
                .map(is_skill_gcop_managed)
                .unwrap_or(false);
            let from_version = existing.as_deref().and_then(skill_managed_version);
            let to_version = normalize_version(current_version);

            if !check {
                atomic_write(target, &content)?;
            }

            if was_managed {
                if check {
                    InstallReportKind::WouldUpgrade {
                        from_version,
                        to_version,
                    }
                } else {
                    InstallReportKind::Upgraded {
                        from_version,
                        to_version,
                    }
                }
            } else if check {
                InstallReportKind::WouldReplace
            } else {
                InstallReportKind::Replaced
            }
        }
        SkillAction::RequireForce { reason } => InstallReportKind::Conflict { reason },
    };

    Ok(InstallReport {
        kind,
        path: target.to_path_buf(),
    })
}

/// Remove a gcop SKILL.md at `target`. Leaves foreign files untouched.
///
/// Returns:
/// - [`UninstallReport::Removed`] if the file existed and was gcop-managed.
/// - [`UninstallReport::SkippedNotFound`] if `target` does not exist.
/// - [`UninstallReport::SkippedForeign`] if `target` exists but is NOT
///   gcop-managed — we never delete user-authored content.
pub fn uninstall_skill(target: &Path) -> Result<UninstallReport> {
    let Some(existing) = read_optional(target)? else {
        return Ok(UninstallReport::SkippedNotFound);
    };
    if !is_skill_gcop_managed(&existing) {
        return Ok(UninstallReport::SkippedForeign);
    }
    fs::remove_file(target).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to remove {}: {}", target.display(), e),
        )
    })?;
    Ok(UninstallReport::Removed)
}

/// Read a file as UTF-8, treating "file not found" as `None` (idempotent
/// installs depend on this distinction). Any other IO error propagates.
fn read_optional(target: &Path) -> Result<Option<String>> {
    match fs::read_to_string(target) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("failed to read {}: {}", target.display(), e),
        )
        .into()),
    }
}

/// Normalize a version string to `"vX.Y.Z"` form for user-facing reports.
fn normalize_version(version: &str) -> String {
    let bare = version.strip_prefix('v').unwrap_or(version);
    format!("v{}", bare)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::render::{render_skill, templates};

    /// Helper: render a real SKILL.md template for `version`.
    fn render(version: &str) -> String {
        render_skill(templates::SKILL_CLAUDE, version).unwrap()
    }

    // ----- Decision table coverage (5 rows) -----

    #[test]
    fn no_file_yields_create() {
        let rendered = render("0.14.0");
        let action = plan_skill_action(None, &rendered, "0.14.0", false);
        assert_eq!(action, SkillAction::Create { content: rendered });
    }

    #[test]
    fn same_version_managed_yields_skip() {
        let rendered = render("0.14.0");
        let action = plan_skill_action(Some(&rendered), &rendered, "0.14.0", false);
        assert_eq!(action, SkillAction::SkipUpToDate);
    }

    #[test]
    fn different_version_managed_yields_replace() {
        let existing = render("0.13.0");
        let rendered = render("0.14.0");
        let action = plan_skill_action(Some(&existing), &rendered, "0.14.0", false);
        assert_eq!(action, SkillAction::Replace { content: rendered });
    }

    #[test]
    fn non_managed_without_force_yields_require_force() {
        let foreign = "---\nname: somebody-elses-skill\n---\nbody\n";
        let rendered = render("0.14.0");
        match plan_skill_action(Some(foreign), &rendered, "0.14.0", false) {
            SkillAction::RequireForce { reason } => {
                assert!(reason.contains("--force"));
            }
            other => panic!("expected RequireForce, got {:?}", other),
        }
    }

    #[test]
    fn non_managed_with_force_yields_replace() {
        let foreign = "---\nname: somebody-elses-skill\n---\nbody\n";
        let rendered = render("0.14.0");
        let action = plan_skill_action(Some(foreign), &rendered, "0.14.0", true);
        assert_eq!(action, SkillAction::Replace { content: rendered });
    }

    // ----- Version normalization -----

    #[test]
    fn version_with_v_prefix_matches_bare_version() {
        // Sentinel stores "v0.14.0"; caller passes "0.14.0".
        let rendered = render("0.14.0");
        // sanity: rendered file actually contains the "v"-prefixed sentinel.
        assert!(rendered.contains("gcop-rs-managed: \"v0.14.0\""));
        let action = plan_skill_action(Some(&rendered), &rendered, "0.14.0", false);
        assert_eq!(action, SkillAction::SkipUpToDate);
    }

    #[test]
    fn version_bare_matches_v_prefixed_existing() {
        // Even if the caller passes "v0.14.0", we should still match.
        let rendered = render("0.14.0");
        let action = plan_skill_action(Some(&rendered), &rendered, "v0.14.0", false);
        assert_eq!(action, SkillAction::SkipUpToDate);
    }

    // ----- Robustness corners -----

    #[test]
    fn empty_existing_treated_as_non_managed_requires_force() {
        let rendered = render("0.14.0");
        let action = plan_skill_action(Some(""), &rendered, "0.14.0", false);
        match action {
            SkillAction::RequireForce { .. } => {}
            other => panic!("expected RequireForce for empty file, got {:?}", other),
        }
    }

    #[test]
    fn corrupted_existing_treated_as_non_managed_requires_force() {
        // Half-formed frontmatter is not "managed" — and we should refuse
        // to silently rewrite it without --force.
        let half = "---\nname: gcop\ngcop-rs-managed: \"v0.13.0\"\n";
        let rendered = render("0.14.0");
        let action = plan_skill_action(Some(half), &rendered, "0.14.0", false);
        match action {
            SkillAction::RequireForce { .. } => {}
            other => panic!(
                "expected RequireForce for malformed frontmatter, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn force_does_not_bypass_skip_up_to_date() {
        // --force is irrelevant when version already matches; we still skip.
        let rendered = render("0.14.0");
        let action = plan_skill_action(Some(&rendered), &rendered, "0.14.0", true);
        assert_eq!(action, SkillAction::SkipUpToDate);
    }

    // =========================================================================
    // install_skill (IO orchestration)
    // =========================================================================

    use std::fs;
    use tempfile::TempDir;

    /// Helper: temp-dir target path for SKILL.md.
    fn target(tmp: &TempDir) -> std::path::PathBuf {
        tmp.path().join("skills").join("gcop").join("SKILL.md")
    }

    #[test]
    fn install_creates_new_file_when_missing() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rendered = render("0.14.0");
        let report = install_skill(&t, &rendered, "0.14.0", false, false).unwrap();
        assert_eq!(report.kind, InstallReportKind::Created);
        assert!(t.exists());
        assert_eq!(fs::read_to_string(&t).unwrap(), rendered);
    }

    #[test]
    fn install_is_idempotent_for_same_version() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rendered = render("0.14.0");
        install_skill(&t, &rendered, "0.14.0", false, false).unwrap();
        let report = install_skill(&t, &rendered, "0.14.0", false, false).unwrap();
        assert_eq!(report.kind, InstallReportKind::UpToDate);
    }

    #[test]
    fn install_upgrades_when_version_differs() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let old = render("0.13.0");
        let new = render("0.14.0");
        install_skill(&t, &old, "0.13.0", false, false).unwrap();
        let report = install_skill(&t, &new, "0.14.0", false, false).unwrap();
        match report.kind {
            InstallReportKind::Upgraded {
                from_version,
                to_version,
            } => {
                assert_eq!(from_version.as_deref(), Some("v0.13.0"));
                assert_eq!(to_version, "v0.14.0");
            }
            other => panic!("expected Upgraded, got {:?}", other),
        }
        // file on disk reflects new content.
        assert_eq!(fs::read_to_string(&t).unwrap(), new);
    }

    #[test]
    fn install_conflict_when_foreign_and_no_force() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        // Prime with a foreign SKILL.md.
        fs::create_dir_all(t.parent().unwrap()).unwrap();
        fs::write(&t, "---\nname: someone-else\n---\n").unwrap();
        let rendered = render("0.14.0");
        let report = install_skill(&t, &rendered, "0.14.0", false, false).unwrap();
        match report.kind {
            InstallReportKind::Conflict { reason } => {
                assert!(reason.contains("--force"));
            }
            other => panic!("expected Conflict, got {:?}", other),
        }
        // File untouched.
        assert!(fs::read_to_string(&t).unwrap().contains("someone-else"));
    }

    #[test]
    fn install_force_overwrites_foreign() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        fs::create_dir_all(t.parent().unwrap()).unwrap();
        fs::write(&t, "---\nname: someone-else\n---\n").unwrap();
        let rendered = render("0.14.0");
        let report = install_skill(&t, &rendered, "0.14.0", true, false).unwrap();
        assert_eq!(report.kind, InstallReportKind::Replaced);
        assert_eq!(fs::read_to_string(&t).unwrap(), rendered);
    }

    #[test]
    fn install_check_mode_does_not_write_for_create() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rendered = render("0.14.0");
        let report = install_skill(&t, &rendered, "0.14.0", false, true).unwrap();
        assert_eq!(report.kind, InstallReportKind::WouldCreate);
        assert!(!t.exists(), "check mode must not touch disk");
    }

    #[test]
    fn install_check_mode_does_not_write_for_upgrade() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let old = render("0.13.0");
        install_skill(&t, &old, "0.13.0", false, false).unwrap();
        let new = render("0.14.0");
        let report = install_skill(&t, &new, "0.14.0", false, true).unwrap();
        match report.kind {
            InstallReportKind::WouldUpgrade { .. } => {}
            other => panic!("expected WouldUpgrade, got {:?}", other),
        }
        // file content still the OLD version
        assert_eq!(fs::read_to_string(&t).unwrap(), old);
    }

    #[test]
    fn install_check_mode_does_not_write_for_force_replace() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        fs::create_dir_all(t.parent().unwrap()).unwrap();
        fs::write(&t, "---\nname: someone-else\n---\n").unwrap();
        let rendered = render("0.14.0");
        let report = install_skill(&t, &rendered, "0.14.0", true, true).unwrap();
        assert_eq!(report.kind, InstallReportKind::WouldReplace);
        assert!(fs::read_to_string(&t).unwrap().contains("someone-else"));
    }

    #[test]
    fn install_check_mode_for_up_to_date_still_reports_up_to_date() {
        // --check on an already-installed file should NOT report
        // WouldCreate/WouldUpgrade — it should report UpToDate.
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rendered = render("0.14.0");
        install_skill(&t, &rendered, "0.14.0", false, false).unwrap();
        let report = install_skill(&t, &rendered, "0.14.0", false, true).unwrap();
        assert_eq!(report.kind, InstallReportKind::UpToDate);
    }

    #[test]
    fn install_check_mode_reports_conflict_without_writing() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        fs::create_dir_all(t.parent().unwrap()).unwrap();
        fs::write(&t, "---\nname: someone-else\n---\n").unwrap();
        let rendered = render("0.14.0");
        let report = install_skill(&t, &rendered, "0.14.0", false, true).unwrap();
        match report.kind {
            InstallReportKind::Conflict { .. } => {}
            other => panic!("expected Conflict, got {:?}", other),
        }
        assert!(fs::read_to_string(&t).unwrap().contains("someone-else"));
    }

    #[test]
    fn install_returns_path_of_target() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rendered = render("0.14.0");
        let report = install_skill(&t, &rendered, "0.14.0", false, true).unwrap();
        assert_eq!(report.path, t);
    }

    // =========================================================================
    // uninstall_skill
    // =========================================================================

    #[test]
    fn uninstall_removes_gcop_managed_file() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rendered = render("0.14.0");
        install_skill(&t, &rendered, "0.14.0", false, false).unwrap();
        let report = uninstall_skill(&t).unwrap();
        assert_eq!(report, UninstallReport::Removed);
        assert!(!t.exists());
    }

    #[test]
    fn uninstall_skips_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let report = uninstall_skill(&t).unwrap();
        assert_eq!(report, UninstallReport::SkippedNotFound);
    }

    #[test]
    fn uninstall_skips_foreign_file_and_leaves_it_intact() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        fs::create_dir_all(t.parent().unwrap()).unwrap();
        let foreign = "---\nname: not-ours\n---\nimportant content\n";
        fs::write(&t, foreign).unwrap();
        let report = uninstall_skill(&t).unwrap();
        assert_eq!(report, UninstallReport::SkippedForeign);
        // file content untouched.
        assert_eq!(fs::read_to_string(&t).unwrap(), foreign);
    }
}
