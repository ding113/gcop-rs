//! CLAUDE.md / AGENTS.md sentinel-block install/uninstall.
//!
//! These files are user-authored — gcop-rs only owns the block delimited
//! by `<!-- gcop-rs:begin v… -->` … `<!-- gcop-rs:end -->`. Every operation
//! preserves the bytes OUTSIDE that block exactly.
//!
//! Layered the same way as `skill_writer`:
//!
//! 1. **Decision** is delegated to
//!    [`crate::commands::agent::sentinel::plan_block_action`] (Step 4).
//! 2. **IO** lives here: read → decide → splice → atomic_write.
//!
//! ## Idempotency, robustness, atomicity
//!
//! - Same version installed twice → `UpToDate` on the second run.
//! - File corrupted (mismatched markers) → returned as `Corrupted`,
//!   absolutely no write occurs.
//! - All file writes go through [`atomic_io::atomic_write`], so crashes
//!   mid-write never leave a half-spliced file.
//! - Uninstall preserves user content; if removing the block empties the
//!   file we delete the file outright instead of leaving a 0-byte stub.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::commands::agent::atomic_io::atomic_write;
use crate::commands::agent::sentinel::{self, BlockAction, plan_block_action};
use crate::error::Result;

/// What `install_block` did at `target`, for user-facing reports.
#[derive(Debug, PartialEq, Eq)]
pub enum BlockInstallReportKind {
    /// No file existed; we wrote a new file containing only the block.
    Created,
    /// File existed without a sentinel; we appended the block at the end.
    /// User content is preserved verbatim above it.
    Appended,
    /// Same version already present; no IO performed.
    UpToDate,
    /// Sentinel block existed at a different version; we spliced the new
    /// block in place, preserving content on both sides.
    Replaced {
        from_version: Option<String>,
        to_version: String,
    },
    /// File is in a half-valid state (mismatched markers, etc.); we
    /// refused to write anything and surface the reason to the user.
    Corrupted {
        reason: String,
    },

    /// `--check` variants: would have done X, but didn't.
    WouldCreate,
    WouldAppend,
    WouldReplace {
        from_version: Option<String>,
        to_version: String,
    },
}

#[derive(Debug)]
pub struct BlockInstallReport {
    pub kind: BlockInstallReportKind,
    pub path: PathBuf,
}

/// What `uninstall_block` did at `target`.
#[derive(Debug, PartialEq, Eq)]
pub enum BlockUninstallReport {
    /// Block was removed and the file rewritten with surrounding content intact.
    Removed,
    /// Block was the entire file content; we deleted the file instead of
    /// leaving an empty stub.
    RemovedAndDeletedEmptyFile,
    /// File did not exist; nothing to do.
    SkippedNotFound,
    /// File exists but contains no gcop-rs block; we touched nothing.
    SkippedNoBlock,
    /// File contains malformed markers; we refused to modify it.
    Corrupted { reason: String },
}

/// Install (or check) a gcop-rs block into `target`.
///
/// Algorithm:
/// 1. Read `target` (or `None` if missing).
/// 2. Call [`plan_block_action`] to compute the action.
/// 3. If `check == false`, perform the IO via [`atomic_write`].
///    Otherwise return a `Would…` variant without touching disk.
pub fn install_block(
    target: &Path,
    rendered_block: &str,
    current_version: &str,
    check: bool,
) -> Result<BlockInstallReport> {
    let existing = read_optional(target)?;
    let action = plan_block_action(existing.as_deref(), rendered_block, current_version);

    let kind = match action {
        BlockAction::Create { content } => {
            if check {
                BlockInstallReportKind::WouldCreate
            } else {
                atomic_write(target, &content)?;
                BlockInstallReportKind::Created
            }
        }
        BlockAction::Append { content } => {
            if check {
                BlockInstallReportKind::WouldAppend
            } else {
                atomic_write(target, &content)?;
                BlockInstallReportKind::Appended
            }
        }
        BlockAction::SkipUpToDate => BlockInstallReportKind::UpToDate,
        BlockAction::Replace {
            begin,
            end_exclusive,
            new_block,
        } => {
            // Safe: plan_block_action returns Replace only when existing is Some.
            let existing_str = existing
                .as_deref()
                .expect("Replace implies existing content");
            let old_block = &existing_str[begin..end_exclusive];
            let from_version = sentinel::extract_block_version(old_block);
            let to_version = normalize_version(current_version);

            if check {
                BlockInstallReportKind::WouldReplace {
                    from_version,
                    to_version,
                }
            } else {
                let spliced = splice_block(existing_str, begin, end_exclusive, &new_block);
                atomic_write(target, &spliced)?;
                BlockInstallReportKind::Replaced {
                    from_version,
                    to_version,
                }
            }
        }
        BlockAction::Corrupted { reason } => BlockInstallReportKind::Corrupted { reason },
    };

    Ok(BlockInstallReport {
        kind,
        path: target.to_path_buf(),
    })
}

/// Remove the gcop-rs block from `target`, preserving any other content.
///
/// If the resulting file would be empty (only whitespace), delete the file
/// outright instead of leaving a 0-byte stub.
pub fn uninstall_block(target: &Path) -> Result<BlockUninstallReport> {
    let Some(content) = read_optional(target)? else {
        return Ok(BlockUninstallReport::SkippedNotFound);
    };

    // Locate the block. We use plan_block_action's helpers indirectly by
    // re-implementing the structural scan here — but with the strictness
    // we want for uninstall (Corrupted is surfaced; missing is fine).
    let begin = content.find(sentinel::BLOCK_BEGIN_PREFIX);
    let end_start = content.find(sentinel::BLOCK_END);

    match (begin, end_start) {
        (None, None) => Ok(BlockUninstallReport::SkippedNoBlock),
        (None, Some(_)) => Ok(BlockUninstallReport::Corrupted {
            reason: format!(
                "found `{}` without matching `{}`",
                sentinel::BLOCK_END,
                sentinel::BLOCK_BEGIN_PREFIX.trim_end()
            ),
        }),
        (Some(_), None) => Ok(BlockUninstallReport::Corrupted {
            reason: format!(
                "found `{}` without matching `{}`",
                sentinel::BLOCK_BEGIN_PREFIX.trim_end(),
                sentinel::BLOCK_END
            ),
        }),
        (Some(b), Some(e)) if b >= e => Ok(BlockUninstallReport::Corrupted {
            reason: format!(
                "`{}` appears before `{}`",
                sentinel::BLOCK_END,
                sentinel::BLOCK_BEGIN_PREFIX.trim_end()
            ),
        }),
        (Some(b), Some(e)) => {
            let end_exclusive = e + sentinel::BLOCK_END.len();
            // Also strip a single trailing newline that follows the end
            // marker so we don't leave a blank line where the block used
            // to be.
            let strip_extra = content.as_bytes().get(end_exclusive) == Some(&b'\n');
            let real_end = end_exclusive + if strip_extra { 1 } else { 0 };

            // Also strip preceding double-newline padding that we inject
            // during Append, so removing the block in a file that ONLY
            // had this block + a separator doesn't leave dangling blank
            // lines.
            let head = &content[..b];
            let tail = &content[real_end..];
            // Collapse multiple trailing newlines in head down to one.
            let head_trimmed = trim_trailing_blank_lines(head);
            let combined = format!("{}{}", head_trimmed, tail);

            if combined.trim().is_empty() {
                // File would be effectively empty; remove it entirely.
                fs::remove_file(target).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!("failed to remove now-empty {}: {}", target.display(), e),
                    )
                })?;
                Ok(BlockUninstallReport::RemovedAndDeletedEmptyFile)
            } else {
                atomic_write(target, &combined)?;
                Ok(BlockUninstallReport::Removed)
            }
        }
    }
}

/// Splice in `new_block` over `[begin..end_exclusive)` of `existing`.
fn splice_block(existing: &str, begin: usize, end_exclusive: usize, new_block: &str) -> String {
    let mut out = String::with_capacity(existing.len() + new_block.len());
    out.push_str(&existing[..begin]);
    out.push_str(new_block);
    out.push_str(&existing[end_exclusive..]);
    out
}

/// Read a file as UTF-8, treating "not found" as `None` so install/
/// uninstall can distinguish "missing" from "I/O error".
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

/// Trim any trailing blank lines (whitespace-only newlines) from `s`,
/// keeping at most one final newline.
fn trim_trailing_blank_lines(s: &str) -> String {
    let trimmed = s.trim_end_matches(['\n', '\r', ' ', '\t']);
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{}\n", trimmed)
    }
}

/// `"0.14.0"` → `"v0.14.0"`, `"v0.14.0"` → `"v0.14.0"`.
fn normalize_version(version: &str) -> String {
    let bare = version.strip_prefix('v').unwrap_or(version);
    format!("v{}", bare)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::render::{render_block, templates};
    use std::fs;
    use tempfile::TempDir;

    fn rendered(version: &str) -> String {
        render_block(templates::INSTRUCTIONS_BLOCK, version).unwrap()
    }

    fn target(tmp: &TempDir) -> PathBuf {
        tmp.path().join("CLAUDE.md")
    }

    // ----------- install_block -----------

    #[test]
    fn install_creates_new_file_when_missing() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", false).unwrap();
        assert_eq!(rep.kind, BlockInstallReportKind::Created);
        let on_disk = fs::read_to_string(&t).unwrap();
        assert!(on_disk.contains("<!-- gcop-rs:begin v0.14.0 -->"));
        assert!(on_disk.contains("<!-- gcop-rs:end -->"));
    }

    #[test]
    fn install_appends_when_existing_has_no_block_preserving_user_content() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let user = "# My personal CLAUDE.md\n\nMy own instructions here.\n";
        fs::write(&t, user).unwrap();
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", false).unwrap();
        assert_eq!(rep.kind, BlockInstallReportKind::Appended);
        let on_disk = fs::read_to_string(&t).unwrap();
        assert!(on_disk.starts_with(user), "user content preserved verbatim");
        assert!(on_disk.contains("<!-- gcop-rs:begin v0.14.0 -->"));
    }

    #[test]
    fn install_idempotent_for_same_version() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        install_block(&t, &rendered("0.14.0"), "0.14.0", false).unwrap();
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", false).unwrap();
        assert_eq!(rep.kind, BlockInstallReportKind::UpToDate);
    }

    #[test]
    fn install_replaces_block_in_place_preserving_surrounding_content() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        // Set up a file with user content surrounding an old block.
        let header = "# Header\n\nimportant header line\n\n";
        let old_block = rendered("0.13.0");
        let footer = "\n## My footer\n\nmore user content\n";
        let initial = format!("{}{}{}", header, old_block, footer);
        fs::write(&t, &initial).unwrap();

        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", false).unwrap();
        match rep.kind {
            BlockInstallReportKind::Replaced {
                from_version,
                to_version,
            } => {
                assert_eq!(from_version.as_deref(), Some("v0.13.0"));
                assert_eq!(to_version, "v0.14.0");
            }
            other => panic!("expected Replaced, got {:?}", other),
        }
        let on_disk = fs::read_to_string(&t).unwrap();
        assert!(on_disk.starts_with(header), "header preserved");
        assert!(on_disk.contains("important header line"));
        assert!(on_disk.contains("more user content"));
        assert!(on_disk.contains("<!-- gcop-rs:begin v0.14.0 -->"));
        assert!(!on_disk.contains("v0.13.0"));
    }

    #[test]
    fn install_corrupted_file_does_not_write() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        // Only BEGIN marker, no END.
        let corrupted = "# header\n<!-- gcop-rs:begin v0.14.0 -->\nbroken\n";
        fs::write(&t, corrupted).unwrap();
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", false).unwrap();
        match rep.kind {
            BlockInstallReportKind::Corrupted { reason } => {
                assert!(reason.contains("without matching"));
            }
            other => panic!("expected Corrupted, got {:?}", other),
        }
        // File untouched.
        assert_eq!(fs::read_to_string(&t).unwrap(), corrupted);
    }

    #[test]
    fn install_check_does_not_write_for_create() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", true).unwrap();
        assert_eq!(rep.kind, BlockInstallReportKind::WouldCreate);
        assert!(!t.exists());
    }

    #[test]
    fn install_check_does_not_write_for_append() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let user = "# My CLAUDE.md\n";
        fs::write(&t, user).unwrap();
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", true).unwrap();
        assert_eq!(rep.kind, BlockInstallReportKind::WouldAppend);
        // Content untouched.
        assert_eq!(fs::read_to_string(&t).unwrap(), user);
    }

    #[test]
    fn install_check_does_not_write_for_replace() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let initial = format!("header\n\n{}\nfooter\n", rendered("0.13.0"));
        fs::write(&t, &initial).unwrap();
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", true).unwrap();
        match rep.kind {
            BlockInstallReportKind::WouldReplace { .. } => {}
            other => panic!("expected WouldReplace, got {:?}", other),
        }
        assert_eq!(fs::read_to_string(&t).unwrap(), initial);
    }

    #[test]
    fn install_path_in_report_matches_target() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let rep = install_block(&t, &rendered("0.14.0"), "0.14.0", true).unwrap();
        assert_eq!(rep.path, t);
    }

    // ----------- uninstall_block -----------

    #[test]
    fn uninstall_removes_block_and_preserves_user_content() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let header = "# Header\n\nimportant\n";
        let block = rendered("0.14.0");
        let footer = "\n## Footer\n\nmore\n";
        let combined = format!("{}\n{}{}", header, block, footer);
        fs::write(&t, &combined).unwrap();

        let rep = uninstall_block(&t).unwrap();
        assert_eq!(rep, BlockUninstallReport::Removed);
        let on_disk = fs::read_to_string(&t).unwrap();
        assert!(on_disk.contains("important"));
        assert!(on_disk.contains("## Footer"));
        assert!(!on_disk.contains("<!-- gcop-rs:begin"));
        assert!(!on_disk.contains("<!-- gcop-rs:end"));
    }

    #[test]
    fn uninstall_deletes_file_when_only_block_remains() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        // File contains only our block (created on a virgin CLAUDE.md).
        fs::write(&t, rendered("0.14.0")).unwrap();
        let rep = uninstall_block(&t).unwrap();
        assert_eq!(rep, BlockUninstallReport::RemovedAndDeletedEmptyFile);
        assert!(!t.exists());
    }

    #[test]
    fn uninstall_skips_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        assert_eq!(
            uninstall_block(&t).unwrap(),
            BlockUninstallReport::SkippedNotFound
        );
    }

    #[test]
    fn uninstall_skips_when_no_block_present() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let user = "# Just my own content\n";
        fs::write(&t, user).unwrap();
        let rep = uninstall_block(&t).unwrap();
        assert_eq!(rep, BlockUninstallReport::SkippedNoBlock);
        // File untouched.
        assert_eq!(fs::read_to_string(&t).unwrap(), user);
    }

    #[test]
    fn uninstall_corrupted_file_does_not_modify_it() {
        let tmp = TempDir::new().unwrap();
        let t = target(&tmp);
        let corrupted = "# header\n<!-- gcop-rs:begin v0.14.0 -->\nstill no end\n";
        fs::write(&t, corrupted).unwrap();
        let rep = uninstall_block(&t).unwrap();
        match rep {
            BlockUninstallReport::Corrupted { reason } => {
                assert!(reason.contains("without matching"));
            }
            other => panic!("expected Corrupted, got {:?}", other),
        }
        assert_eq!(fs::read_to_string(&t).unwrap(), corrupted);
    }

    // ----------- normalize_version (helper) -----------

    #[test]
    fn normalize_version_adds_v_prefix() {
        assert_eq!(normalize_version("0.14.0"), "v0.14.0");
    }

    #[test]
    fn normalize_version_idempotent() {
        assert_eq!(normalize_version("v0.14.0"), "v0.14.0");
    }
}
