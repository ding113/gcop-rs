//! Template rendering for agent integration files.
//!
//! Two pure renderers:
//!
//! - [`render_skill`] takes a SKILL.md template (must start with YAML
//!   frontmatter `---`) and injects the gcop-rs version as
//!   `gcop-rs-managed: "v{version}"` immediately before the closing `---`.
//! - [`render_block`] wraps a CLAUDE.md/AGENTS.md body fragment in
//!   `<!-- gcop-rs:begin v{version} -->` / `<!-- gcop-rs:end -->` markers.
//!
//! Both are **pure**: no IO, no env reads. They take strings, return
//! strings. The IO layer (`skill_writer.rs`, `instructions_writer.rs`)
//! decides whether to actually persist.
//!
//! The bundled templates themselves live in [`templates`] as
//! `include_str!`-baked constants, so the binary remains a single static
//! file with no runtime resource discovery.

use crate::commands::agent::sentinel::{BLOCK_BEGIN_PREFIX, BLOCK_END, SKILL_SENTINEL_KEY};
use crate::error::{GcopError, Result};

/// Compile-time templates baked into the binary by `include_str!`.
///
/// Higher layers (`claude.rs`, `codex.rs`) pick the right constant for the
/// agent they're integrating with. Both skill templates are valid input
/// for [`render_skill`]; the instructions block is valid input for
/// [`render_block`].
pub mod templates {
    /// SKILL.md template for Claude Code (`~/.claude/skills/gcop/SKILL.md`).
    pub const SKILL_CLAUDE: &str = include_str!("templates/skill-claude.md");

    /// SKILL.md template for Codex (`~/.codex/skills/gcop/SKILL.md`).
    pub const SKILL_CODEX: &str = include_str!("templates/skill-codex.md");

    /// Body of the always-loaded prompt block injected into CLAUDE.md and
    /// AGENTS.md. The renderer wraps it in `<!-- gcop-rs:begin/end -->`.
    pub const INSTRUCTIONS_BLOCK: &str = include_str!("templates/instructions-block.md");
}

/// Inject `gcop-rs-managed: "v{version}"` into the SKILL.md template's
/// frontmatter and return the full rendered file content.
///
/// # Errors
///
/// - Template does not begin with `---` (no frontmatter) → [`GcopError::Config`].
/// - Template has opening `---` but no closing `---` → [`GcopError::Config`].
///
/// These errors only fire during development if a template file is broken.
/// In production they signal a build/embed problem worth surfacing loudly.
pub fn render_skill(template: &str, version: &str) -> Result<String> {
    let mut lines: Vec<String> = template.lines().map(str::to_string).collect();

    // Frontmatter must open with `---` on the very first line.
    let first_is_marker = lines.first().map(|l| l.trim() == "---").unwrap_or(false);
    if !first_is_marker {
        return Err(GcopError::Config(
            "SKILL.md template must begin with `---` frontmatter marker".to_string(),
        ));
    }

    // Find the closing `---`.
    let close_idx = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i)
        .ok_or_else(|| {
            GcopError::Config(
                "SKILL.md template has opening `---` but no closing `---`".to_string(),
            )
        })?;

    let sentinel = format!("{}: \"v{}\"", SKILL_SENTINEL_KEY, version);
    lines.insert(close_idx, sentinel);

    // Reconstruct, preserving the original trailing-newline behavior.
    // `str::lines()` discards the final newline if any; we add it back when
    // the original ended in one.
    let mut out = lines.join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Wrap `body` in `<!-- gcop-rs:begin v{version} -->` / `<!-- gcop-rs:end -->`.
///
/// Used for CLAUDE.md and AGENTS.md inserts. Returned string always ends
/// with the closing marker and a trailing newline.
///
/// `body` may or may not have trailing newlines; the renderer normalizes so
/// there is exactly one blank line between the body and the end marker is
/// not required (the marker sits on its own line right after the body).
pub fn render_block(body: &str, version: &str) -> Result<String> {
    if version.is_empty() {
        return Err(GcopError::Config(
            "render_block: version must not be empty".to_string(),
        ));
    }

    // Normalize: ensure body has exactly one trailing newline so the end
    // marker is on its own line.
    let trimmed_body = body.trim_end_matches('\n');

    Ok(format!(
        "{}v{} -->\n{}\n{}\n",
        BLOCK_BEGIN_PREFIX, version, trimmed_body, BLOCK_END
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent::sentinel::{
        BlockAction, is_skill_gcop_managed, plan_block_action, skill_managed_version,
    };

    // ------------------------- render_skill ----------------------------------

    #[test]
    fn render_skill_injects_sentinel_into_minimal_template() {
        let template = "---\nname: foo\n---\nbody\n";
        let rendered = render_skill(template, "0.14.0").unwrap();
        assert!(rendered.contains("gcop-rs-managed: \"v0.14.0\""));
        // sentinel must be detected by the production sentinel scanner.
        assert!(is_skill_gcop_managed(&rendered));
        assert_eq!(skill_managed_version(&rendered).as_deref(), Some("v0.14.0"));
    }

    #[test]
    fn render_skill_preserves_template_body() {
        let template = "---\nname: foo\n---\n# Body\n\nparagraph\n";
        let rendered = render_skill(template, "1.2.3").unwrap();
        assert!(rendered.contains("# Body"));
        assert!(rendered.contains("paragraph"));
    }

    #[test]
    fn render_skill_preserves_trailing_newline() {
        let with_nl = "---\nname: x\n---\nbody\n";
        let without_nl = "---\nname: x\n---\nbody";
        assert!(render_skill(with_nl, "0.0.1").unwrap().ends_with('\n'));
        assert!(!render_skill(without_nl, "0.0.1").unwrap().ends_with('\n'));
    }

    #[test]
    fn render_skill_rejects_missing_opening_marker() {
        let template = "name: foo\n---\nbody\n";
        let err = render_skill(template, "0.14.0").unwrap_err();
        match err {
            GcopError::Config(msg) => assert!(msg.contains("begin with `---`")),
            other => panic!("expected Config error, got {:?}", other),
        }
    }

    #[test]
    fn render_skill_rejects_missing_closing_marker() {
        let template = "---\nname: foo\nno closer here\n";
        let err = render_skill(template, "0.14.0").unwrap_err();
        match err {
            GcopError::Config(msg) => {
                assert!(msg.contains("no closing `---`"), "got: {}", msg);
            }
            other => panic!("expected Config error, got {:?}", other),
        }
    }

    #[test]
    fn render_skill_idempotent_across_versions() {
        // Same template + different version → both detected, versions differ.
        let template = "---\nname: foo\n---\nbody\n";
        let v1 = render_skill(template, "0.14.0").unwrap();
        let v2 = render_skill(template, "0.15.0").unwrap();
        assert!(is_skill_gcop_managed(&v1));
        assert!(is_skill_gcop_managed(&v2));
        assert_eq!(skill_managed_version(&v1).as_deref(), Some("v0.14.0"));
        assert_eq!(skill_managed_version(&v2).as_deref(), Some("v0.15.0"));
        assert_ne!(v1, v2);
    }

    // ------------------------- render_block ----------------------------------

    #[test]
    fn render_block_wraps_body_with_markers() {
        let rendered = render_block("Hello body", "0.14.0").unwrap();
        assert!(rendered.starts_with("<!-- gcop-rs:begin v0.14.0 -->\n"));
        assert!(rendered.ends_with("<!-- gcop-rs:end -->\n"));
        assert!(rendered.contains("Hello body"));
    }

    #[test]
    fn render_block_normalizes_trailing_newlines_in_body() {
        let a = render_block("body", "0.14.0").unwrap();
        let b = render_block("body\n", "0.14.0").unwrap();
        let c = render_block("body\n\n\n", "0.14.0").unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn render_block_rejects_empty_version() {
        let err = render_block("body", "").unwrap_err();
        match err {
            GcopError::Config(msg) => assert!(msg.contains("version")),
            other => panic!("expected Config error, got {:?}", other),
        }
    }

    #[test]
    fn render_block_round_trips_with_plan_block_action() {
        // Rendering a block, then asking plan_block_action what to do with
        // a file that contains exactly that block + same version, must yield
        // SkipUpToDate (proving sentinel.rs and render.rs agree on format).
        let rendered = render_block("body", "0.14.0").unwrap();
        let file = format!("user header\n\n{}", rendered);
        let new = render_block("body v2", "0.14.0").unwrap();
        assert_eq!(
            plan_block_action(Some(&file), &new, "v0.14.0"),
            BlockAction::SkipUpToDate
        );
    }

    // ------------------------- bundled-template sanity -----------------------

    /// Templates that gcop-rs ships MUST contain certain user-facing
    /// guarantees verbatim. If a future edit accidentally removes them,
    /// this test fails loud and clear.
    #[test]
    fn bundled_skill_claude_contains_required_guarantees() {
        let rendered = render_skill(templates::SKILL_CLAUDE, "0.14.0").unwrap();
        assert!(
            rendered.contains("gcop-rs commit --split -y"),
            "Claude skill must instruct `commit --split -y`"
        );
        assert!(
            rendered.contains("200000"),
            "Claude skill must mention 200000ms (200s) timeout"
        );
        assert!(is_skill_gcop_managed(&rendered));
    }

    #[test]
    fn bundled_skill_codex_contains_required_guarantees() {
        let rendered = render_skill(templates::SKILL_CODEX, "0.14.0").unwrap();
        assert!(
            rendered.contains("gcop-rs commit --split -y"),
            "Codex skill must instruct `commit --split -y`"
        );
        assert!(
            rendered.contains("200000"),
            "Codex skill must mention 200000ms (200s) timeout"
        );
        assert!(is_skill_gcop_managed(&rendered));
    }

    #[test]
    fn bundled_instructions_block_contains_required_guarantees() {
        let rendered = render_block(templates::INSTRUCTIONS_BLOCK, "0.14.0").unwrap();
        assert!(
            rendered.contains("gcop-rs commit --split -y"),
            "Instructions block must instruct `commit --split -y`"
        );
        assert!(
            rendered.contains("200000"),
            "Instructions block must mention 200000ms (200s) timeout"
        );
        // round-trip into plan_block_action — same version yields SkipUpToDate
        assert_eq!(
            plan_block_action(Some(&rendered), &rendered, "v0.14.0"),
            BlockAction::SkipUpToDate
        );
    }

    #[test]
    fn bundled_skill_templates_are_well_formed_frontmatter() {
        // Constants must always parse — protect against unintentional edits
        // that break the `---\n...\n---` shape.
        assert!(render_skill(templates::SKILL_CLAUDE, "0.0.0").is_ok());
        assert!(render_skill(templates::SKILL_CODEX, "0.0.0").is_ok());
    }
}
