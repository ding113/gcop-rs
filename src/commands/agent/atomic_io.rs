//! Filesystem primitives used by `skill_writer` and `instructions_writer`.
//!
//! Two functions:
//!
//! - [`ensure_parent_dir`] — `mkdir -p`-style parent directory creation.
//!   No-op if the parent already exists. Surfaces an `io::Error` with a
//!   contextual message if it cannot create the directory.
//! - [`atomic_write`] — writes `content` to `target` in a way that readers
//!   never observe partial content, even under `Ctrl-C` or concurrent
//!   writers. Implemented via `tempfile::NamedTempFile::persist`, which
//!   performs a same-filesystem rename atomically.
//!
//! All other layers in `agent/` MUST go through these functions for any
//! file write — there is no direct `fs::write` elsewhere in the module.
//! Reads still use `fs::read_to_string` because partial-read semantics
//! cannot deceive a caller that diff-checks before writing.

use crate::error::Result;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use tempfile::NamedTempFile;

/// Ensure that `target`'s parent directory exists. Creates all intermediate
/// directories if necessary. No-op if the parent already exists.
///
/// Errors are wrapped to include the directory we were trying to create,
/// so a permission failure says *which* directory was unwritable.
pub fn ensure_parent_dir(target: &Path) -> Result<()> {
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    // An empty parent (`PathBuf::new().parent()` returns Some("")) means
    // "current directory" and definitely exists.
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to create directory {}: {}", parent.display(), e),
        )
    })?;
    Ok(())
}

/// Write `content` to `target` atomically. Creates `target`'s parent
/// directory if missing.
///
/// Guarantees:
///
/// - Readers of `target` never observe partial content.
/// - On crash or `Ctrl-C` mid-write, no stale half-file remains: the temp
///   is auto-unlinked by [`NamedTempFile`]'s `Drop`.
/// - Concurrent writes are safe: last writer wins atomically.
///
/// Requires same-filesystem rename, which we guarantee by creating the
/// temporary file in `target`'s parent directory.
pub fn atomic_write(target: &Path, content: &str) -> Result<()> {
    ensure_parent_dir(target)?;

    // Default to "." if target has no parent (e.g. just a bare filename
    // in CWD). create_dir_all on "." is a no-op, so this is safe.
    let parent = target.parent().unwrap_or_else(|| Path::new("."));

    let mut tmp = NamedTempFile::new_in(parent).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "failed to create temporary file in {}: {}",
                parent.display(),
                e
            ),
        )
    })?;

    tmp.write_all(content.as_bytes())
        .map_err(|e| io::Error::new(e.kind(), format!("failed to write temporary file: {}", e)))?;

    tmp.flush()
        .map_err(|e| io::Error::new(e.kind(), format!("failed to flush temporary file: {}", e)))?;

    tmp.persist(target).map_err(|persist_err| {
        io::Error::new(
            persist_err.error.kind(),
            format!(
                "failed to rename temporary file to {}: {}",
                target.display(),
                persist_err.error
            ),
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ---- ensure_parent_dir ----

    #[test]
    fn ensure_parent_dir_creates_missing_parent() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("a").join("b").join("c").join("file.md");
        ensure_parent_dir(&nested).unwrap();
        assert!(nested.parent().unwrap().is_dir());
    }

    #[test]
    fn ensure_parent_dir_existing_parent_is_noop() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("file.md");
        ensure_parent_dir(&target).unwrap();
        // Second call still ok.
        ensure_parent_dir(&target).unwrap();
        assert!(tmp.path().is_dir());
    }

    #[test]
    fn ensure_parent_dir_for_bare_filename_is_ok() {
        // `Path::new("file.md").parent()` returns Some("") — must not error.
        let bare = Path::new("file.md");
        ensure_parent_dir(bare).unwrap();
    }

    // ---- atomic_write ----

    #[test]
    fn atomic_write_creates_new_file() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("nested").join("file.md");
        atomic_write(&target, "hello world\n").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello world\n");
    }

    #[test]
    fn atomic_write_overwrites_existing_file() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("file.md");
        atomic_write(&target, "first").unwrap();
        atomic_write(&target, "second").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "second");
    }

    #[test]
    fn atomic_write_does_not_leave_tempfiles_on_success() {
        // After a successful write, parent dir should contain only `target`
        // — no leftover tempfile entries from the rename.
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("file.md");
        atomic_write(&target, "ok").unwrap();
        let entries: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected just `file.md`, got {:?}",
            entries
        );
        assert_eq!(entries[0], std::ffi::OsString::from("file.md"));
    }

    #[test]
    fn atomic_write_creates_deep_nested_parent() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("a").join("b").join("c").join("file.md");
        atomic_write(&target, "deep").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "deep");
    }

    #[test]
    fn atomic_write_empty_content_is_valid() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("file.md");
        atomic_write(&target, "").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "");
    }

    #[test]
    fn atomic_write_unicode_content_is_preserved() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("file.md");
        let content = "你好 🦀 hello\n";
        atomic_write(&target, content).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), content);
    }

    /// Failure isolation: writing into a read-only parent must fail
    /// cleanly without leaving a half-written `target` file.
    ///
    /// Unix-only — Windows file ACL is too divergent to write a single
    /// portable test, and gcop-rs's CI matrix covers the Unix case which
    /// is by far the more common deployment.
    #[cfg(unix)]
    #[test]
    fn atomic_write_failure_leaves_no_half_file() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let ro_dir = tmp.path().join("readonly");
        fs::create_dir(&ro_dir).unwrap();
        // Strip write permission from the directory.
        let mut perms = fs::metadata(&ro_dir).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&ro_dir, perms).unwrap();

        let target = ro_dir.join("file.md");
        let result = atomic_write(&target, "data");
        // Restore permissions so TempDir can clean up.
        let mut restore = fs::metadata(&ro_dir).unwrap().permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&ro_dir, restore).unwrap();

        assert!(result.is_err(), "expected error on read-only parent");
        assert!(!target.exists(), "no half-file should remain on failure");
    }
}
