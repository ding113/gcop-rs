//! Cross-platform path resolution for coding-agent integration.
//!
//! Resolves the on-disk locations of the per-agent files we manage:
//!
//! | Agent | Skill dir                          | Always-on prompt file |
//! |-------|------------------------------------|-----------------------|
//! | Claude| `~/.claude/skills/gcop/`           | `~/.claude/CLAUDE.md` |
//! | Codex | `~/.codex/skills/gcop/`            | `~/.codex/AGENTS.md`  |
//!
//! Both agents store config under `~/.<agent>/` rather than the OS standard
//! config dir (XDG_CONFIG_HOME / `%APPDATA%`), so we anchor on `home_dir()`
//! and append the agent's literal subdirectory.
//!
//! # Env overrides
//!
//! For tests and power users:
//!
//! - `GCOP_CLAUDE_DIR` — overrides `~/.claude` for both skill dir and CLAUDE.md.
//! - `CODEX_HOME` — overrides `~/.codex` for both skill dir and AGENTS.md.
//!
//! These take precedence over `home_dir()`. If the override is set to a path
//! that does not yet exist, resolution still succeeds — callers (`atomic_io`)
//! are responsible for `create_dir_all` before writing.

use crate::error::{GcopError, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const SKILL_LEAF: &str = "skills/gcop";
const CLAUDE_INSTRUCTIONS_LEAF: &str = "CLAUDE.md";
const CODEX_INSTRUCTIONS_LEAF: &str = "AGENTS.md";

const CLAUDE_ENV_OVERRIDE: &str = "GCOP_CLAUDE_DIR";
const CODEX_ENV_OVERRIDE: &str = "CODEX_HOME";

const CLAUDE_HOME_SUBDIR: &str = ".claude";
const CODEX_HOME_SUBDIR: &str = ".codex";

/// Resolve the Claude Code config root: env override, else `~/.claude/`.
fn claude_root() -> Result<PathBuf> {
    if let Some(v) = env_path(CLAUDE_ENV_OVERRIDE) {
        return Ok(v);
    }
    Ok(home_dir()?.join(CLAUDE_HOME_SUBDIR))
}

/// Resolve the Codex config root: env override, else `~/.codex/`.
fn codex_root() -> Result<PathBuf> {
    if let Some(v) = env_path(CODEX_ENV_OVERRIDE) {
        return Ok(v);
    }
    Ok(home_dir()?.join(CODEX_HOME_SUBDIR))
}

/// `~/.claude/skills/gcop/` (or `$GCOP_CLAUDE_DIR/skills/gcop/`).
pub fn resolve_claude_skill_dir() -> Result<PathBuf> {
    Ok(claude_root()?.join(Path::new(SKILL_LEAF)))
}

/// `~/.claude/CLAUDE.md` (or `$GCOP_CLAUDE_DIR/CLAUDE.md`).
pub fn resolve_claude_md() -> Result<PathBuf> {
    Ok(claude_root()?.join(CLAUDE_INSTRUCTIONS_LEAF))
}

/// `~/.codex/skills/gcop/` (or `$CODEX_HOME/skills/gcop/`).
pub fn resolve_codex_skill_dir() -> Result<PathBuf> {
    Ok(codex_root()?.join(Path::new(SKILL_LEAF)))
}

/// `~/.codex/AGENTS.md` (or `$CODEX_HOME/AGENTS.md`).
pub fn resolve_codex_agents_md() -> Result<PathBuf> {
    Ok(codex_root()?.join(CODEX_INSTRUCTIONS_LEAF))
}

/// Look up an env var as a `PathBuf`. Returns `None` if unset OR empty
/// (so a user clearing the override with `KEY=` works as expected).
fn env_path(key: &str) -> Option<PathBuf> {
    let raw: OsString = std::env::var_os(key)?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

/// Resolve the user's home directory.
///
/// Uses `directories::UserDirs` — works on macOS/Linux/Windows. Returns
/// [`GcopError::Config`] if the platform refuses to give us one (extremely
/// rare; only happens in broken sandboxes).
fn home_dir() -> Result<PathBuf> {
    directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| {
            GcopError::Config(
                "Cannot resolve user home directory. Set GCOP_CLAUDE_DIR or CODEX_HOME explicitly."
                    .to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;
    use tempfile::TempDir;

    /// RAII guard that sets an env var on construction and restores the
    /// prior value on drop. Necessary because Rust 1.82+ requires `set_var`
    /// / `remove_var` to be wrapped in `unsafe` (concurrent set_var can
    /// data-race global env state).
    struct EnvGuard {
        key: &'static str,
        prev: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let prev = std::env::var_os(key);
            // SAFETY: tests using EnvGuard are wrapped in #[serial], so no
            // other test mutates the same env var concurrently.
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }

        fn unset(key: &'static str) -> Self {
            let prev = std::env::var_os(key);
            // SAFETY: see Self::set.
            unsafe { std::env::remove_var(key) };
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see Self::set.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    // -------- GCOP_CLAUDE_DIR override --------

    #[test]
    #[serial]
    fn claude_skill_dir_uses_env_override() {
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(CLAUDE_ENV_OVERRIDE, tmp.path());

        let p = resolve_claude_skill_dir().unwrap();
        assert_eq!(p, tmp.path().join("skills").join("gcop"));
    }

    #[test]
    #[serial]
    fn claude_md_uses_env_override() {
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(CLAUDE_ENV_OVERRIDE, tmp.path());

        let p = resolve_claude_md().unwrap();
        assert_eq!(p, tmp.path().join("CLAUDE.md"));
    }

    #[test]
    #[serial]
    fn claude_paths_share_root_under_override() {
        // skill dir and CLAUDE.md must share the same prefix — they are
        // both "two files in the same claude root".
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(CLAUDE_ENV_OVERRIDE, tmp.path());

        let skill = resolve_claude_skill_dir().unwrap();
        let md = resolve_claude_md().unwrap();
        assert!(skill.starts_with(tmp.path()));
        assert!(md.starts_with(tmp.path()));
    }

    #[test]
    #[serial]
    fn claude_skill_dir_falls_back_to_home() {
        let _g = EnvGuard::unset(CLAUDE_ENV_OVERRIDE);

        let p = resolve_claude_skill_dir().unwrap();
        let suffix: PathBuf = [".claude", "skills", "gcop"].iter().collect();
        assert!(
            p.ends_with(&suffix),
            "expected path ending with {:?}, got {:?}",
            suffix,
            p,
        );
    }

    #[test]
    #[serial]
    fn claude_md_falls_back_to_home() {
        let _g = EnvGuard::unset(CLAUDE_ENV_OVERRIDE);

        let p = resolve_claude_md().unwrap();
        let suffix: PathBuf = [".claude", "CLAUDE.md"].iter().collect();
        assert!(
            p.ends_with(&suffix),
            "expected path ending with {:?}, got {:?}",
            suffix,
            p,
        );
    }

    #[test]
    #[serial]
    fn claude_empty_env_var_is_ignored() {
        // GCOP_CLAUDE_DIR= (set but empty) should NOT override — it's a
        // common shell pattern for "clear this override".
        let _g = EnvGuard::set(CLAUDE_ENV_OVERRIDE, Path::new(""));

        let p = resolve_claude_skill_dir().unwrap();
        let suffix: PathBuf = [".claude", "skills", "gcop"].iter().collect();
        assert!(
            p.ends_with(&suffix),
            "empty env should fall through to home; got {:?}",
            p,
        );
    }

    // -------- CODEX_HOME override --------

    #[test]
    #[serial]
    fn codex_skill_dir_uses_env_override() {
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(CODEX_ENV_OVERRIDE, tmp.path());

        let p = resolve_codex_skill_dir().unwrap();
        assert_eq!(p, tmp.path().join("skills").join("gcop"));
    }

    #[test]
    #[serial]
    fn codex_agents_md_uses_env_override() {
        let tmp = TempDir::new().unwrap();
        let _g = EnvGuard::set(CODEX_ENV_OVERRIDE, tmp.path());

        let p = resolve_codex_agents_md().unwrap();
        assert_eq!(p, tmp.path().join("AGENTS.md"));
    }

    #[test]
    #[serial]
    fn codex_skill_dir_falls_back_to_home() {
        let _g = EnvGuard::unset(CODEX_ENV_OVERRIDE);

        let p = resolve_codex_skill_dir().unwrap();
        let suffix: PathBuf = [".codex", "skills", "gcop"].iter().collect();
        assert!(
            p.ends_with(&suffix),
            "expected path ending with {:?}, got {:?}",
            suffix,
            p,
        );
    }

    #[test]
    #[serial]
    fn codex_agents_md_falls_back_to_home() {
        let _g = EnvGuard::unset(CODEX_ENV_OVERRIDE);

        let p = resolve_codex_agents_md().unwrap();
        let suffix: PathBuf = [".codex", "AGENTS.md"].iter().collect();
        assert!(
            p.ends_with(&suffix),
            "expected path ending with {:?}, got {:?}",
            suffix,
            p,
        );
    }

    #[test]
    #[serial]
    fn codex_empty_env_var_is_ignored() {
        let _g = EnvGuard::set(CODEX_ENV_OVERRIDE, Path::new(""));

        let p = resolve_codex_skill_dir().unwrap();
        let suffix: PathBuf = [".codex", "skills", "gcop"].iter().collect();
        assert!(
            p.ends_with(&suffix),
            "empty env should fall through to home; got {:?}",
            p,
        );
    }

    // -------- agent isolation --------

    #[test]
    #[serial]
    fn claude_and_codex_overrides_are_independent() {
        // Setting GCOP_CLAUDE_DIR must NOT affect codex resolution and vice versa.
        let claude_tmp = TempDir::new().unwrap();
        let codex_tmp = TempDir::new().unwrap();
        let _g1 = EnvGuard::set(CLAUDE_ENV_OVERRIDE, claude_tmp.path());
        let _g2 = EnvGuard::set(CODEX_ENV_OVERRIDE, codex_tmp.path());

        assert!(
            resolve_claude_skill_dir()
                .unwrap()
                .starts_with(claude_tmp.path()),
        );
        assert!(
            resolve_codex_skill_dir()
                .unwrap()
                .starts_with(codex_tmp.path()),
        );
        assert_ne!(claude_tmp.path(), codex_tmp.path());
    }
}
