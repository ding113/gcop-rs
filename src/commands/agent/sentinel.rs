//! Sentinel detection for files managed by gcop-rs.
//!
//! gcop-rs marks every file it writes with a sentinel so that future
//! installs (idempotent upgrade) can distinguish "previously written by us"
//! from "user-authored content with the same name".
//!
//! Two sentinel formats coexist because the file types accept different
//! syntaxes:
//!
//! | File type            | Sentinel format            | Why                          |
//! |----------------------|----------------------------|------------------------------|
//! | SKILL.md             | YAML key in frontmatter    | First line MUST be `---`     |
//! | CLAUDE.md / AGENTS.md| HTML comment block         | No frontmatter constraint    |
//!
//! Both detectors are **pure functions over `&str`** — no IO, no env, fully
//! testable. IO and decision orchestration live in `skill_writer.rs` and
//! `instructions_writer.rs`.
//!
//! # Step 3 scope
//!
//! This module currently implements only the SKILL.md (YAML key) sentinel.
//! Step 4 adds the HTML comment block sentinel for CLAUDE.md / AGENTS.md.

// =============================================================================
// SKILL.md sentinel — YAML key in frontmatter
// =============================================================================

/// YAML key used to mark a SKILL.md as gcop-rs-managed.
///
/// Embedded into the frontmatter like:
///
/// ```yaml
/// ---
/// name: gcop
/// description: ...
/// gcop-rs-managed: "v0.14.0"
/// ---
/// ```
///
/// The key is intentionally hyphenated and contains the word "managed" to
/// minimize collision risk with user metadata. YAML key names are
/// case-sensitive per spec.
pub const SKILL_SENTINEL_KEY: &str = "gcop-rs-managed";

/// True if `content` is a SKILL.md actively managed by gcop-rs.
///
/// Decision rule (intentionally strict for safety):
///
/// 1. The file must have a well-formed YAML frontmatter — both an opening
///    `---` line and a closing `---` line.
/// 2. A line inside that frontmatter must start with `gcop-rs-managed:`.
/// 3. Sentinel-like lines OUTSIDE the frontmatter (in the markdown body)
///    do not count — they could be example code blocks or documentation.
/// 4. Look-alike keys (`gcop-rs-managed-by:`, `not-gcop-rs-managed:`) do
///    not count — strict equality on the key segment.
///
/// Why strict: a half-malformed file should NOT be silently overwritten on
/// `agent install`. The decision layer (`skill_writer::plan_action`) treats
/// "exists but not managed" as `RequireForce`, which is the right safety net.
pub fn is_skill_gcop_managed(content: &str) -> bool {
    scan_skill_frontmatter(content).is_some()
}

/// Returns the version string declared by the sentinel, if any.
///
/// Parses lines like `gcop-rs-managed: "v0.14.0"` or `gcop-rs-managed: v0.14.0`
/// inside a well-formed YAML frontmatter. Returns `None` if no sentinel
/// found, frontmatter is malformed, or the value is empty.
///
/// Used by `skill_writer::plan_action` to decide SkipUpToDate vs. ReplaceFile.
pub fn skill_managed_version(content: &str) -> Option<String> {
    scan_skill_frontmatter(content)
}

/// Walks `content` looking for a well-formed YAML frontmatter that contains
/// a `gcop-rs-managed:` line. Returns the parsed version, or `None`.
///
/// Implementation note: we deliberately do NOT use a YAML parser here.
/// Reasons:
/// - A YAML parser would refuse to parse malformed frontmatter, masking the
///   diagnostic value of "we know there's a half-formed frontmatter".
/// - The scan is line-based and trivially correct.
/// - Pulling in a YAML dependency just for this check would be overkill;
///   `serde_yaml_ng` is already a dep but using it would conflate "parse
///   yaml" with "scan for sentinel".
fn scan_skill_frontmatter(content: &str) -> Option<String> {
    let mut in_frontmatter = false;
    let mut found: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "---" {
            if !in_frontmatter {
                in_frontmatter = true;
                continue;
            }
            // Closing `---` — return whatever we found in the frontmatter
            // (None if no sentinel, Some(version) if one was present).
            return found;
        }

        if in_frontmatter && is_sentinel_line(trimmed) {
            found = extract_sentinel_version(trimmed);
        }
    }

    // EOF reached without a closing `---`. Treat as malformed — not managed.
    None
}

/// True if `trimmed` is a `gcop-rs-managed:` key declaration.
///
/// Must match the EXACT key followed by `:` to reject look-alikes like
/// `gcop-rs-managed-by:` or `gcop-rs-managed_extra:`.
fn is_sentinel_line(trimmed: &str) -> bool {
    let key_with_colon = match trimmed.split_once(':') {
        Some((key, _)) => key,
        None => return false,
    };
    key_with_colon == SKILL_SENTINEL_KEY
}

/// Parse `gcop-rs-managed: "v0.14.0"` → `Some("v0.14.0".to_string())`.
///
/// Handles:
/// - `key: "value"` (double-quoted)
/// - `key: 'value'` (single-quoted)
/// - `key: value` (bare)
/// - `key: ` (empty value) → `None`
fn extract_sentinel_version(sentinel_line: &str) -> Option<String> {
    let after_colon = sentinel_line.split_once(':')?.1.trim();
    let unquoted = after_colon
        .trim_matches(|c: char| c == '"' || c == '\'')
        .trim();
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

// =============================================================================
// CLAUDE.md / AGENTS.md sentinel — HTML comment block
// =============================================================================

/// Marker that opens a gcop-rs-managed block. The full opening line is
/// `<!-- gcop-rs:begin vX.Y.Z -->` where the version follows the prefix.
pub const BLOCK_BEGIN_PREFIX: &str = "<!-- gcop-rs:begin ";

/// Marker that closes a gcop-rs-managed block.
pub const BLOCK_END: &str = "<!-- gcop-rs:end -->";

/// Decision returned by [`plan_block_action`] for `install_block` callers
/// to execute. Each variant carries everything needed to perform the IO —
/// the IO layer never re-parses content.
#[derive(Debug, PartialEq, Eq)]
pub enum BlockAction {
    /// File does not exist on disk. Write a new file containing only the
    /// rendered block (plus trailing newline normalisation).
    Create { content: String },

    /// File exists but contains no gcop-rs sentinel. Append the block at
    /// the end, preserving every byte of pre-existing user content. The
    /// payload is the full new file body (existing + separator + block).
    Append { content: String },

    /// File exists, sentinel is present, version matches current. Nothing
    /// to do; report "up to date" to the user.
    SkipUpToDate,

    /// File exists, sentinel is present, version differs. Splice the
    /// existing block out and the new block in at `[begin..end_exclusive)`.
    /// The byte indices are into the *original* file content.
    Replace {
        begin: usize,
        end_exclusive: usize,
        new_block: String,
    },

    /// File is in a half-valid state we refuse to "fix" automatically.
    /// IO layer must surface `reason` and abort without touching the file.
    Corrupted { reason: String },
}

/// Pure decision: given the current on-disk content (or `None` if the file
/// does not exist), the rendered new block, and the version we're trying
/// to install, return what action to take.
///
/// **Never** performs IO. **Never** mutates inputs. All edge cases produce
/// a deterministic [`BlockAction`].
///
/// `new_block` is expected to already be wrapped in BEGIN/END markers with
/// the current version embedded — the `render` module produces these.
pub fn plan_block_action(
    file_content: Option<&str>,
    new_block: &str,
    current_version: &str,
) -> BlockAction {
    let content = match file_content {
        None => {
            return BlockAction::Create {
                content: normalize_trailing_newline(new_block),
            };
        }
        Some(s) => s,
    };

    let begin_positions = find_all_positions(content, BLOCK_BEGIN_PREFIX);
    let end_positions = find_all_positions(content, BLOCK_END);

    // ---- Validate structure first; corruption gates everything else. ----
    if begin_positions.len() > 1 {
        return BlockAction::Corrupted {
            reason: format!(
                "found {} `{}` markers; expected at most 1",
                begin_positions.len(),
                BLOCK_BEGIN_PREFIX.trim_end()
            ),
        };
    }
    if end_positions.len() > 1 {
        return BlockAction::Corrupted {
            reason: format!(
                "found {} `{}` markers; expected at most 1",
                end_positions.len(),
                BLOCK_END
            ),
        };
    }
    match (begin_positions.first(), end_positions.first()) {
        (None, None) => {
            // No sentinel at all — append.
            BlockAction::Append {
                content: append_with_separator(content, new_block),
            }
        }
        (Some(_), None) => BlockAction::Corrupted {
            reason: format!(
                "found `{}` without matching `{}`",
                BLOCK_BEGIN_PREFIX.trim_end(),
                BLOCK_END
            ),
        },
        (None, Some(_)) => BlockAction::Corrupted {
            reason: format!(
                "found `{}` without matching `{}`",
                BLOCK_END,
                BLOCK_BEGIN_PREFIX.trim_end()
            ),
        },
        (Some(&begin), Some(&end_start)) => {
            if begin >= end_start {
                return BlockAction::Corrupted {
                    reason: format!(
                        "`{}` appears before `{}`",
                        BLOCK_END,
                        BLOCK_BEGIN_PREFIX.trim_end()
                    ),
                };
            }
            let end_exclusive = end_start + BLOCK_END.len();
            let existing_block = &content[begin..end_exclusive];
            // Compare versions case-sensitively but treat leading `v` as
            // optional, so callers can pass either `"0.14.0"` or
            // `"v0.14.0"` and get the same result as the stored sentinel.
            match extract_block_version(existing_block) {
                Some(existing) if block_versions_equal(&existing, current_version) => {
                    BlockAction::SkipUpToDate
                }
                _ => BlockAction::Replace {
                    begin,
                    end_exclusive,
                    new_block: new_block.to_string(),
                },
            }
        }
    }
}

/// Compare two version strings, treating a leading `v` as optional. So
/// `"v0.14.0"` matches `"0.14.0"` matches `"v0.14.0"`.
fn block_versions_equal(a: &str, b: &str) -> bool {
    let a = a.strip_prefix('v').unwrap_or(a);
    let b = b.strip_prefix('v').unwrap_or(b);
    a == b
}

/// Find every non-overlapping position where `needle` appears in `haystack`.
fn find_all_positions(haystack: &str, needle: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(needle) {
        let abs = start + rel;
        positions.push(abs);
        start = abs + needle.len();
    }
    positions
}

/// Parse the version off an existing block: looks at the first line and
/// returns the substring between `BLOCK_BEGIN_PREFIX` and ` -->`.
///
/// `pub(crate)` so `instructions_writer` can record the previous version
/// during a `Replace` for user-facing report messages. Outside callers
/// should prefer [`plan_block_action`] which performs full validation.
pub(crate) fn extract_block_version(block: &str) -> Option<String> {
    let first_line = block.lines().next()?;
    let after_prefix = first_line.strip_prefix(BLOCK_BEGIN_PREFIX)?;
    let version = after_prefix.trim_end_matches("-->").trim();
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

/// Build "existing content + separator + new_block + trailing \n".
///
/// Separator policy:
/// - Empty existing → no leading separator (file is just the block).
/// - Existing already ends in `\n\n` → no extra (would over-pad).
/// - Existing ends in single `\n` → one more `\n`.
/// - Existing ends with non-`\n` → `\n\n`.
fn append_with_separator(existing: &str, new_block: &str) -> String {
    // Separator policy distilled into a flat decision (avoids
    // clippy::if_same_then_else for the two zero-separator cases):
    // - Empty OR already-double-newline → no extra separator.
    // - Single trailing `\n` → one more `\n`.
    // - No trailing newline at all → `\n\n`.
    let separator = if existing.is_empty() || existing.ends_with("\n\n") {
        ""
    } else if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let mut out = String::with_capacity(existing.len() + separator.len() + new_block.len() + 1);
    out.push_str(existing);
    out.push_str(separator);
    out.push_str(new_block);
    if !new_block.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Ensure the string ends with exactly one `\n` (used for `Create` content).
fn normalize_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{}\n", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------- is_skill_gcop_managed -------------------------

    #[test]
    fn empty_content_not_managed() {
        assert!(!is_skill_gcop_managed(""));
    }

    #[test]
    fn no_frontmatter_not_managed() {
        let content = "# Plain markdown\n\nNo frontmatter here.\n";
        assert!(!is_skill_gcop_managed(content));
    }

    #[test]
    fn frontmatter_without_sentinel_not_managed() {
        let content = "---\nname: foo\ndescription: bar\n---\nbody\n";
        assert!(!is_skill_gcop_managed(content));
    }

    #[test]
    fn frontmatter_with_sentinel_managed() {
        let content = "---\n\
                       name: gcop\n\
                       description: ...\n\
                       gcop-rs-managed: \"v0.14.0\"\n\
                       ---\n\
                       # body\n";
        assert!(is_skill_gcop_managed(content));
    }

    #[test]
    fn unclosed_frontmatter_not_managed_even_with_sentinel() {
        // Opening --- but no closing --- → malformed → safer to treat
        // as not-managed so we don't silently rewrite a broken file.
        let content = "---\nname: gcop\ngcop-rs-managed: \"v0.14.0\"\n";
        assert!(!is_skill_gcop_managed(content));
    }

    #[test]
    fn sentinel_in_body_not_counted() {
        // The string `gcop-rs-managed:` appears, but in the markdown body,
        // not in frontmatter. Must not be treated as managed.
        let content = "---\nname: foo\n---\n\n## Example\n\
                       Here's how a managed skill looks: `gcop-rs-managed: \"v0.14.0\"`\n";
        assert!(!is_skill_gcop_managed(content));
    }

    #[test]
    fn lookalike_key_with_suffix_not_managed() {
        let content = "---\ngcop-rs-managed-by: someone\nname: foo\n---\nbody\n";
        assert!(!is_skill_gcop_managed(content));
    }

    #[test]
    fn lookalike_key_with_underscore_not_managed() {
        let content = "---\ngcop_rs_managed: \"v0.14.0\"\nname: foo\n---\nbody\n";
        assert!(!is_skill_gcop_managed(content));
    }

    #[test]
    fn case_sensitive_key() {
        // YAML keys are case-sensitive; "GCOP-RS-MANAGED" is a different key.
        let content = "---\nGCOP-RS-MANAGED: \"v0.14.0\"\nname: foo\n---\nbody\n";
        assert!(!is_skill_gcop_managed(content));
    }

    #[test]
    fn sentinel_with_extra_keys_managed() {
        let content = "---\n\
                       name: gcop\n\
                       description: long desc\n\
                       allowed-tools: Bash\n\
                       license: MIT\n\
                       gcop-rs-managed: \"v0.14.0\"\n\
                       ---\n\
                       body\n";
        assert!(is_skill_gcop_managed(content));
    }

    #[test]
    fn frontmatter_marker_with_whitespace_recognised() {
        // YAML allows trailing whitespace on the `---` line.
        let content = "---  \n\
                       gcop-rs-managed: \"v1.0.0\"\n\
                       ---\n";
        assert!(is_skill_gcop_managed(content));
    }

    #[test]
    fn three_dashes_inside_body_does_not_re_open_frontmatter() {
        // After the closing ---, a stray "---" in the body should NOT
        // re-enter frontmatter and accidentally count a sentinel in the body.
        let content = "---\nname: foo\n---\n\
                       Some text.\n---\n\
                       gcop-rs-managed: \"v0.14.0\"\n";
        // scan_skill_frontmatter exits at the FIRST closing ---, returning
        // the (None) result it found inside the real frontmatter.
        assert!(!is_skill_gcop_managed(content));
    }

    // -------------------------- skill_managed_version ------------------------

    #[test]
    fn version_double_quoted() {
        let content = "---\ngcop-rs-managed: \"v0.14.0\"\n---\n";
        assert_eq!(skill_managed_version(content).as_deref(), Some("v0.14.0"));
    }

    #[test]
    fn version_single_quoted() {
        let content = "---\ngcop-rs-managed: 'v0.14.0'\n---\n";
        assert_eq!(skill_managed_version(content).as_deref(), Some("v0.14.0"));
    }

    #[test]
    fn version_bare_unquoted() {
        let content = "---\ngcop-rs-managed: v0.14.0\n---\n";
        assert_eq!(skill_managed_version(content).as_deref(), Some("v0.14.0"));
    }

    #[test]
    fn version_missing_returns_none() {
        let content = "---\nname: foo\n---\n";
        assert_eq!(skill_managed_version(content), None);
    }

    #[test]
    fn version_empty_value_returns_none() {
        // `gcop-rs-managed: ` with no value → managed=false because we
        // cannot reason about upgrade direction.
        let content = "---\ngcop-rs-managed: \"\"\n---\n";
        assert_eq!(skill_managed_version(content), None);
    }

    #[test]
    fn is_managed_and_version_agree_on_well_formed_input() {
        // Sanity: a content that is_managed() == true MUST also yield Some(version).
        let content = "---\ngcop-rs-managed: \"v9.9.9\"\n---\n";
        assert!(is_skill_gcop_managed(content));
        assert_eq!(skill_managed_version(content).as_deref(), Some("v9.9.9"));
    }

    // -------------------------- plan_block_action ----------------------------

    /// Build a complete BEGIN/body/END block matching the production format.
    fn make_block(version: &str, body: &str) -> String {
        format!(
            "<!-- gcop-rs:begin v{} -->\n{}\n<!-- gcop-rs:end -->",
            version, body
        )
    }

    #[test]
    fn block_no_file_yields_create() {
        let new = make_block("0.14.0", "## Gcop block");
        let action = plan_block_action(None, &new, "v0.14.0");
        let expected = format!("{}\n", new); // normalize_trailing_newline
        assert_eq!(action, BlockAction::Create { content: expected });
    }

    #[test]
    fn block_no_sentinel_yields_append_preserving_user_content() {
        let user = "# My CLAUDE.md\n\nSome existing instructions.\n";
        let new = make_block("0.14.0", "gcop body");
        let action = plan_block_action(Some(user), &new, "v0.14.0");
        match action {
            BlockAction::Append { content } => {
                assert!(
                    content.starts_with(user),
                    "user content prefix must be preserved"
                );
                assert!(
                    content.contains(&new),
                    "new block must appear in appended file"
                );
                // After single `\n`, separator is one extra `\n`.
                assert!(content.contains("\n\n<!-- gcop-rs:begin"));
            }
            other => panic!("expected Append, got {:?}", other),
        }
    }

    #[test]
    fn block_append_empty_existing_does_not_prepend_blank_line() {
        let new = make_block("0.14.0", "gcop");
        let action = plan_block_action(Some(""), &new, "v0.14.0");
        match action {
            BlockAction::Append { content } => {
                // Empty existing → file starts directly with the block.
                assert!(content.starts_with("<!-- gcop-rs:begin"));
            }
            other => panic!("expected Append, got {:?}", other),
        }
    }

    #[test]
    fn block_append_existing_no_trailing_newline_adds_double() {
        let user = "no trailing newline";
        let new = make_block("0.14.0", "gcop");
        let action = plan_block_action(Some(user), &new, "v0.14.0");
        match action {
            BlockAction::Append { content } => {
                assert!(content.starts_with(user));
                assert!(content.contains("\n\n<!-- gcop-rs:begin"));
            }
            other => panic!("expected Append, got {:?}", other),
        }
    }

    #[test]
    fn block_append_existing_double_newline_no_extra_padding() {
        let user = "ends with two newlines\n\n";
        let new = make_block("0.14.0", "gcop");
        let action = plan_block_action(Some(user), &new, "v0.14.0");
        match action {
            BlockAction::Append { content } => {
                // Should NOT contain `\n\n\n` — i.e., we didn't over-pad.
                assert!(!content.contains("\n\n\n"));
                assert!(content.contains("\n\n<!-- gcop-rs:begin"));
            }
            other => panic!("expected Append, got {:?}", other),
        }
    }

    #[test]
    fn block_same_version_yields_skip_up_to_date() {
        let existing_block = make_block("0.14.0", "old body");
        let file = format!("# header\n\n{}\n", existing_block);
        let new = make_block("0.14.0", "new body");
        let action = plan_block_action(Some(&file), &new, "v0.14.0");
        assert_eq!(action, BlockAction::SkipUpToDate);
    }

    #[test]
    fn block_different_version_yields_replace() {
        let existing_block = make_block("0.13.0", "old body");
        let file = format!("# header\n\n{}\n\nuser tail content\n", existing_block);
        let new = make_block("0.14.0", "new body");
        let action = plan_block_action(Some(&file), &new, "v0.14.0");
        match action {
            BlockAction::Replace {
                begin,
                end_exclusive,
                new_block,
            } => {
                // The spliced range must match the existing block bytes exactly.
                let spliced = &file[begin..end_exclusive];
                assert_eq!(spliced, existing_block);
                assert_eq!(new_block, new);
                // Surrounding content must remain.
                let head = &file[..begin];
                let tail = &file[end_exclusive..];
                assert!(head.starts_with("# header"));
                assert!(tail.contains("user tail content"));
            }
            other => panic!("expected Replace, got {:?}", other),
        }
    }

    #[test]
    fn block_only_begin_yields_corrupted() {
        let file = "# header\n<!-- gcop-rs:begin v0.14.0 -->\nleftover body\n";
        let new = make_block("0.14.0", "body");
        match plan_block_action(Some(file), &new, "v0.14.0") {
            BlockAction::Corrupted { reason } => {
                assert!(reason.contains("without matching"), "got: {}", reason);
            }
            other => panic!("expected Corrupted, got {:?}", other),
        }
    }

    #[test]
    fn block_only_end_yields_corrupted() {
        let file = "# header\n<!-- gcop-rs:end -->\n";
        let new = make_block("0.14.0", "body");
        match plan_block_action(Some(file), &new, "v0.14.0") {
            BlockAction::Corrupted { reason } => {
                assert!(reason.contains("without matching"), "got: {}", reason);
            }
            other => panic!("expected Corrupted, got {:?}", other),
        }
    }

    #[test]
    fn block_multiple_begins_yields_corrupted() {
        let blk = make_block("0.14.0", "body");
        let file = format!("{}\n\n{}\n", blk, blk); // two complete blocks
        let new = make_block("0.14.0", "new");
        match plan_block_action(Some(&file), &new, "v0.14.0") {
            BlockAction::Corrupted { reason } => {
                assert!(reason.contains("expected at most 1"), "got: {}", reason);
            }
            other => panic!("expected Corrupted, got {:?}", other),
        }
    }

    #[test]
    fn block_end_before_begin_yields_corrupted() {
        let file = "<!-- gcop-rs:end -->\nstuff\n<!-- gcop-rs:begin v0.14.0 -->\n";
        let new = make_block("0.14.0", "body");
        match plan_block_action(Some(file), &new, "v0.14.0") {
            BlockAction::Corrupted { reason } => {
                assert!(
                    reason.contains("appears before"),
                    "expected order-mismatch reason, got: {}",
                    reason
                );
            }
            other => panic!("expected Corrupted, got {:?}", other),
        }
    }

    #[test]
    fn block_version_extraction_handles_v_prefix() {
        // BLOCK_BEGIN_PREFIX is "<!-- gcop-rs:begin "; the version that
        // follows it includes the leading "v". So "v0.14.0" is the literal
        // string that the version comparison sees.
        let blk = "<!-- gcop-rs:begin v0.14.0 -->\nbody\n<!-- gcop-rs:end -->";
        assert_eq!(extract_block_version(blk).as_deref(), Some("v0.14.0"));
    }

    #[test]
    fn block_version_extraction_returns_none_for_empty() {
        let blk = "<!-- gcop-rs:begin  -->\nbody\n<!-- gcop-rs:end -->";
        assert_eq!(extract_block_version(blk), None);
    }
}
