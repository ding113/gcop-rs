use crate::config::{CommitConvention, ConventionStyle};
use crate::llm::{CommitContext, ReviewType, ScopeInfo};

/// Built-in commit-message system prompt.
///
/// Loaded at compile time from `prompts/commit_system.txt` so the prompt
/// content stays editable as a plain text file — no Rust raw-string
/// escaping concerns, no IDE syntax-highlight confusion, and the prompt
/// can be reviewed in diff form like any other text asset.
///
/// Users fully REPLACE this default by setting `[commit].custom_prompt`
/// in `~/.config/gcop/config.toml` (or any project-level
/// `.gcop/config.toml`); when that field is `None`, this builtin is used.
/// The optional `[commit.convention].extra_prompt` is always appended on
/// top, regardless of which system prompt is active.
const COMMIT_SYSTEM_PROMPT: &str = include_str!("prompts/commit_system.txt");

/// Review basic system commands (can be overridden by customization)
const REVIEW_SYSTEM_PROMPT_BASE: &str = r#"You are an expert code reviewer.

Review criteria:
1. Correctness: bugs or logical errors
2. Security: vulnerabilities
3. Performance: issues
4. Maintainability: readability
5. Best practices"#;

/// JSON format constraints (always appended)
const REVIEW_JSON_CONSTRAINT: &str = r#"

Output JSON format:
{
  "summary": "Brief assessment",
  "issues": [{"severity": "critical|warning|info", "description": "...", "file": "...", "line": N}],
  "suggestions": ["..."]
}"#;

/// Format user feedback list
fn format_feedbacks(feedbacks: &[String]) -> String {
    if feedbacks.is_empty() {
        return String::new();
    }
    let mut result = String::from("\n\n## User Requirements:\n");
    for (i, fb) in feedbacks.iter().enumerate() {
        result.push_str(&format!("{}. {}\n", i + 1, fb));
    }
    result
}

/// Format historical commit-message reference list.
///
/// Renders each entry as a numbered item. When an entry contains body lines
/// (subject + `"\n\n"` + body), the body lines are indented by 3 spaces so
/// CommonMark parsers (and LLMs that mimic that semantic) keep the body as
/// continuation of the list item instead of starting a new top-level
/// paragraph. Returns an empty string when `examples` is empty so the prompt
/// has no orphan header.
fn format_historical_examples(examples: &[String]) -> String {
    if examples.is_empty() {
        return String::new();
    }
    let mut result = String::from("\n\n## Project commit-style references (newest first):\n");
    for (i, ex) in examples.iter().enumerate() {
        result.push_str(&render_history_entry(i + 1, ex));
        result.push_str("\n\n");
    }
    result.trim_end().to_string() + "\n"
}

/// Render one historical-example entry as a numbered list item. The first
/// line becomes the list-item heading; subsequent lines are indented 3
/// spaces so they survive markdown's "blank line terminates list" rule.
fn render_history_entry(index: usize, ex: &str) -> String {
    let mut lines = ex.lines();
    let first = lines.next().unwrap_or("");
    let mut out = format!("{}. {}", index, first);
    for line in lines {
        out.push('\n');
        // Always emit the 3-space continuation prefix, even for blank lines,
        // so the body stays anchored to the list item across paragraph breaks.
        if line.is_empty() {
            out.push_str("  ");
        } else {
            out.push_str("   ");
            out.push_str(line);
        }
    }
    out
}

/// Formatting convention constraint to prompt fragment
fn format_convention(convention: &CommitConvention) -> String {
    let mut parts = Vec::new();

    match convention.style {
        ConventionStyle::Conventional => {
            parts.push("Follow conventional commits format: type(scope): description".to_string());
        }
        ConventionStyle::Gitmoji => {
            parts.push("Use gitmoji format: :emoji: description".to_string());
        }
        ConventionStyle::Custom => {}
    }

    if let Some(ref types) = convention.types {
        parts.push(format!("Allowed types: {}", types.join(", ")));
    }

    if let Some(ref template) = convention.template {
        parts.push(format!("Commit template: {}", template));
    }

    if let Some(ref extra) = convention.extra_prompt {
        parts.push(extra.clone());
    }

    if parts.is_empty() {
        return String::new();
    }

    format!("\n\n## Convention:\n{}", parts.join("\n"))
}

/// Format workspace scope information into prompt fragment
fn format_scope_info(scope: &ScopeInfo) -> String {
    let mut parts = Vec::new();

    if !scope.workspace_types.is_empty() {
        parts.push(format!(
            "Monorepo type: {}",
            scope.workspace_types.join(", ")
        ));
    }

    if !scope.packages.is_empty() {
        parts.push(format!("Affected packages: {}", scope.packages.join(", ")));
    }

    if let Some(ref suggested) = scope.suggested_scope {
        parts.push(format!(
            "Suggested scope for commit message: \"{}\"",
            suggested
        ));
    }

    if scope.has_root_changes {
        parts.push("Note: Some changes are in root-level files (outside any package)".to_string());
    }

    if parts.is_empty() {
        return String::new();
    }

    format!("\n\n## Workspace:\n{}", parts.join("\n"))
}

/// Build context section shared by both normal and split commit prompts.
fn build_context_section(context: &CommitContext) -> String {
    let branch_info = context
        .branch_name
        .as_ref()
        .map(|b| format!("\nBranch: {}", b))
        .unwrap_or_default();

    let scope_section = context
        .scope_info
        .as_ref()
        .map(format_scope_info)
        .unwrap_or_default();

    format!(
        "{}{}{}{}",
        branch_info,
        scope_section,
        format_feedbacks(&context.user_feedback),
        format_historical_examples(&context.historical_examples),
    )
}

/// Build normal commit prompt in system/user split format.
///
/// Return (system_prompt, user_message)
/// - system_prompt: static command, can be cached by LLM
/// - user_message: dynamic content (diff + context + feedback)
pub fn build_commit_prompt_split(
    diff: &str,
    context: &CommitContext,
    custom_template: Option<&str>,
    convention: Option<&CommitConvention>,
) -> (String, String) {
    // Custom template used as system prompt
    let mut system = custom_template.unwrap_or(COMMIT_SYSTEM_PROMPT).to_string();

    // Add convention constraints
    if let Some(conv) = convention {
        system.push_str(&format_convention(conv));
    }

    // user message contains dynamic content
    let user = format!(
        "## Diff:\n```\n{}\n```\n\n## Context:\nFiles: {}\nChanges: +{} -{}{}",
        diff,
        context.files_changed.join(", "),
        context.insertions,
        context.deletions,
        build_context_section(context)
    );

    (system, user)
}

/// Build review prompt in system/user split format.
///
/// Return (system_prompt, user_message)
/// - system_prompt: custom template (or default) + JSON format constraints (always appended)
/// - user_message: Code to be reviewed
pub fn build_review_prompt_split(
    diff: &str,
    _review_type: &ReviewType,
    custom_template: Option<&str>,
) -> (String, String) {
    // Custom template used as base system prompt, always appended with JSON constraints
    let base = custom_template.unwrap_or(REVIEW_SYSTEM_PROMPT_BASE);
    let system = format!("{}{}", base, REVIEW_JSON_CONSTRAINT);

    let user = format!("## Code to Review:\n```\n{}\n```", diff);

    (system, user)
}

/// System prompt for split commit grouping
/// Additional system directives for split (atomic) commit mode.
/// Appended after `COMMIT_SYSTEM_PROMPT` to add grouping + JSON output requirements.
const SPLIT_COMMIT_EXTRA_PROMPT: &str = r#"

You are also a git commit analyzer that groups file changes into logical atomic commits.

CRITICAL CONSTRAINTS (violating these will cause hard errors):
- EACH FILE MUST APPEAR IN EXACTLY ONE GROUP. Listing the same file path in multiple groups is STRICTLY FORBIDDEN.
- Every file in the provided list must be assigned to exactly one group - do not omit any files.

Grouping rules:
- Group related file changes together into logical commits
- Each group represents ONE logical change (feature, bugfix, refactor, etc.)
- Order groups by dependency (foundational changes first)
- If all files are logically related, put them in a single group
- Output ONLY valid JSON, no explanation or markdown fences

Output format:
{
  "groups": [
    {
      "files": ["path/to/file1.rs", "path/to/file2.rs"],
      "message": "type(scope): description"
    }
  ]
}"#;

/// Build split commit prompt (system + user)
///
/// Returns `(system_prompt, user_message)`.
/// The system prompt combines base commit rules with split-specific grouping instructions.
/// The user message contains per-file diffs and context information.
pub fn build_split_commit_prompt(
    file_diffs: &[crate::git::diff::FileDiff],
    context: &CommitContext,
    custom_template: Option<&str>,
    convention: Option<&CommitConvention>,
) -> (String, String) {
    // Base commit rules + split-specific grouping instructions
    let mut system = format!("{}{}", COMMIT_SYSTEM_PROMPT, SPLIT_COMMIT_EXTRA_PROMPT);

    // Append user's custom prompt as additional constraints (not replace)
    if let Some(custom) = custom_template {
        system.push_str("\n\nAdditional instructions:\n");
        system.push_str(custom);
    }

    if let Some(conv) = convention {
        system.push_str(&format_convention(conv));
    }

    // Build user message with per-file diffs
    // Prepend a complete file list so the LLM sees the full partition set upfront.
    let mut user =
        String::from("## Complete file list (each file must appear in EXACTLY ONE group):\n");
    for fd in file_diffs {
        user.push_str(&format!("- {}\n", fd.filename));
    }
    user.push_str("\n## File diffs:\n\n");

    for fd in file_diffs {
        user.push_str(&format!(
            "### File: {} (+{} -{})\n```diff\n{}\n```\n\n",
            fd.filename, fd.insertions, fd.deletions, fd.content
        ));
    }

    let total_insertions: usize = file_diffs.iter().map(|f| f.insertions).sum();
    let total_deletions: usize = file_diffs.iter().map(|f| f.deletions).sum();

    user.push_str(&format!(
        "## Context:\nTotal files: {}\nTotal changes: +{} -{}{}",
        file_diffs.len(),
        total_insertions,
        total_deletions,
        build_context_section(context)
    ));

    (system, user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn create_context(
        files: Vec<&str>,
        insertions: usize,
        deletions: usize,
        branch: Option<&str>,
        feedbacks: Vec<&str>,
    ) -> CommitContext {
        CommitContext {
            files_changed: files.into_iter().map(String::from).collect(),
            insertions,
            deletions,
            branch_name: branch.map(String::from),
            custom_prompt: None,
            user_feedback: feedbacks.into_iter().map(String::from).collect(),
            convention: None,
            scope_info: None,
            historical_examples: Vec::new(),
        }
    }

    // === build_commit_prompt_split test ===

    #[test]
    fn test_commit_prompt_split_default() {
        let ctx = create_context(vec!["foo.rs"], 10, 5, None, vec![]);
        let (system, user) = build_commit_prompt_split("diff content", &ctx, None, None);

        // system should contain the builtin role + format references
        assert!(system.contains("expert software engineer"));
        assert!(system.contains("Conventional Commits"));

        // user should contain diff and context
        assert!(user.contains("diff content"));
        assert!(user.contains("foo.rs"));
        assert!(user.contains("+10 -5"));
    }

    #[test]
    fn test_commit_prompt_split_with_branch() {
        let ctx = create_context(vec!["a.rs"], 1, 1, Some("feature/test"), vec![]);
        let (_, user) = build_commit_prompt_split("diff", &ctx, None, None);

        assert!(user.contains("Branch: feature/test"));
    }

    #[test]
    fn test_commit_prompt_split_with_feedback() {
        let ctx = create_context(
            vec!["a.rs"],
            1,
            1,
            None,
            vec!["请使用中文", "不要超过50字符"],
        );
        let (_, user) = build_commit_prompt_split("diff", &ctx, None, None);

        assert!(user.contains("User Requirements"));
        assert!(user.contains("1. 请使用中文"));
        assert!(user.contains("2. 不要超过50字符"));
    }

    #[test]
    fn test_commit_prompt_split_custom_template() {
        let ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        let (system, _) =
            build_commit_prompt_split("diff", &ctx, Some("Custom system prompt"), None);

        // Custom template replaces system prompt for normal commit
        assert_eq!(system, "Custom system prompt");
    }

    // === convention injection test ===

    #[test]
    fn test_commit_prompt_split_with_conventional_convention() {
        let ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        let conv = CommitConvention {
            style: ConventionStyle::Conventional,
            types: Some(vec!["feat".to_string(), "fix".to_string()]),
            ..Default::default()
        };
        let (system, _) = build_commit_prompt_split("diff", &ctx, None, Some(&conv));

        assert!(system.contains("## Convention:"));
        assert!(system.contains("conventional commits"));
        assert!(system.contains("Allowed types: feat, fix"));
    }

    #[test]
    fn test_commit_prompt_split_with_gitmoji_convention() {
        let ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        let conv = CommitConvention {
            style: ConventionStyle::Gitmoji,
            ..Default::default()
        };
        let (system, _) = build_commit_prompt_split("diff", &ctx, None, Some(&conv));

        assert!(system.contains("gitmoji"));
    }

    #[test]
    fn test_commit_prompt_split_with_custom_convention() {
        let ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        let conv = CommitConvention {
            style: ConventionStyle::Custom,
            template: Some("{type}({scope}): {subject}".to_string()),
            extra_prompt: Some("Use English only".to_string()),
            ..Default::default()
        };
        let (system, _) = build_commit_prompt_split("diff", &ctx, None, Some(&conv));

        assert!(system.contains("Commit template: {type}({scope}): {subject}"));
        assert!(system.contains("Use English only"));
    }

    #[test]
    fn test_commit_prompt_split_no_convention() {
        let ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        let (system_with, _) = build_commit_prompt_split("diff", &ctx, None, None);
        // The Convention section should not be included when there is no convention
        assert!(!system_with.contains("## Convention:"));
    }

    // === build_split_commit_prompt custom template test ===

    #[test]
    fn test_split_commit_prompt_custom_template_appended() {
        let ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        let diffs = vec![crate::git::diff::FileDiff {
            filename: "a.rs".to_string(),
            content: "+code".to_string(),
            insertions: 1,
            deletions: 1,
        }];
        let (system, _) = build_split_commit_prompt(&diffs, &ctx, Some("Use Japanese"), None);

        // Base commit rules must be present
        assert!(system.contains("Conventional Commits"));
        // Split grouping rules must be present
        assert!(system.contains("groups"));
        assert!(system.contains("JSON"));
        // Custom prompt appended, not replacing
        assert!(system.contains("Additional instructions:"));
        assert!(system.contains("Use Japanese"));
    }

    #[test]
    fn test_split_commit_prompt_uses_lockfile_summary() {
        let ctx = create_context(vec!["Cargo.lock"], 42, 7, None, vec![]);
        let diffs = vec![crate::git::diff::FileDiff {
            filename: "Cargo.lock".to_string(),
            content: "diff --git a/Cargo.lock b/Cargo.lock\n+lots of lock content".to_string(),
            insertions: 42,
            deletions: 7,
        }];
        let (diffs, changed) = crate::commands::summarize_lockfile_diffs(&diffs, &[]);

        assert!(changed);
        let (_, user) = build_split_commit_prompt(&diffs, &ctx, None, None);

        assert!(user.contains("- Cargo.lock"));
        assert!(user.contains("### File: Cargo.lock (+42 -7)"));
        assert!(user.contains("Lockfile diff omitted; summary only: +42 -7 lines"));
        assert!(!user.contains("+lots of lock content"));
    }

    // === build_review_prompt_split test ===

    #[test]
    fn test_review_prompt_split_default() {
        let (system, user) =
            build_review_prompt_split("code diff", &ReviewType::UncommittedChanges, None);

        // system should contain review rules and JSON format
        assert!(system.contains("code reviewer"));
        assert!(system.contains("JSON format"));

        // user should contain code
        assert!(user.contains("code diff"));
        assert!(user.contains("Code to Review"));
    }

    #[test]
    fn test_review_prompt_split_custom_template() {
        let (system, _) =
            build_review_prompt_split("diff", &ReviewType::UncommittedChanges, Some("Custom"));

        // Custom template + JSON constraints are always appended
        assert!(system.starts_with("Custom"));
        assert!(system.contains("JSON format"));
        assert!(system.contains("\"summary\""));
    }

    // === scope info injection test ===

    #[test]
    fn test_commit_prompt_with_scope_info() {
        let ctx = CommitContext {
            files_changed: vec!["packages/core/src/lib.rs".into()],
            insertions: 5,
            deletions: 2,
            branch_name: None,
            custom_prompt: None,
            user_feedback: vec![],
            convention: None,
            scope_info: Some(ScopeInfo {
                workspace_types: vec!["cargo".into()],
                packages: vec!["packages/core".into()],
                suggested_scope: Some("core".into()),
                has_root_changes: false,
            }),
            historical_examples: Vec::new(),
        };
        let (_, user) = build_commit_prompt_split("diff", &ctx, None, None);

        assert!(user.contains("## Workspace:"));
        assert!(user.contains("Monorepo type: cargo"));
        assert!(user.contains("Affected packages: packages/core"));
        assert!(user.contains("Suggested scope for commit message: \"core\""));
        assert!(!user.contains("root-level"));
    }

    #[test]
    fn test_commit_prompt_without_scope_info() {
        let ctx = create_context(vec!["src/main.rs"], 1, 1, None, vec![]);
        let (_, user) = build_commit_prompt_split("diff", &ctx, None, None);

        assert!(!user.contains("## Workspace:"));
    }

    // === Historical examples injection (Iteration G) ===

    #[test]
    fn test_format_historical_examples_empty_returns_empty_string() {
        assert_eq!(format_historical_examples(&[]), "");
    }

    #[test]
    fn test_format_historical_examples_numbered_list() {
        let out = format_historical_examples(&["feat: a".into(), "fix: b".into()]);
        assert!(out.contains("## Project commit-style references"));
        assert!(out.contains("1. feat: a"));
        assert!(out.contains("2. fix: b"));
    }

    #[test]
    fn test_commit_prompt_split_includes_history_section() {
        let mut ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        ctx.historical_examples = vec!["feat: x".into(), "fix(scope): y".into()];
        let (system, user) = build_commit_prompt_split("diff", &ctx, None, None);

        // History lives in the user message (cacheable system stays clean)
        assert!(!system.contains("Project commit-style references"));
        assert!(user.contains("Project commit-style references"));
        assert!(user.contains("feat: x"));
        assert!(user.contains("fix(scope): y"));
    }

    #[test]
    fn test_split_commit_prompt_also_includes_history_section() {
        let mut ctx = create_context(vec!["a.rs"], 1, 1, None, vec![]);
        ctx.historical_examples = vec!["chore: bump deps".into()];
        let diffs = vec![crate::git::diff::FileDiff {
            filename: "a.rs".to_string(),
            content: "+code".to_string(),
            insertions: 1,
            deletions: 1,
        }];
        let (system, user) = build_split_commit_prompt(&diffs, &ctx, None, None);
        assert!(!system.contains("Project commit-style references"));
        assert!(user.contains("Project commit-style references"));
        assert!(user.contains("chore: bump deps"));
    }

    #[test]
    fn test_format_historical_examples_indents_multi_line_body() {
        // Multi-line body should be indented by 3 spaces so CommonMark sees
        // it as list-item continuation, not a new top-level paragraph.
        let ex = "feat: foo\n\nbody line 1\nbody line 2".to_string();
        let out = format_historical_examples(&[ex]);
        assert!(out.contains("1. feat: foo"));
        // Each body line must be indented with 3 spaces.
        assert!(out.contains("   body line 1"));
        assert!(out.contains("   body line 2"));
        // The unindented form must NOT appear.
        assert!(!out.contains("\nbody line 1"));
    }

    #[test]
    fn test_format_historical_examples_multi_entry_with_bodies() {
        let entries = vec![
            "feat: alpha\n\nfirst body".to_string(),
            "fix: beta\n\nsecond body".to_string(),
        ];
        let out = format_historical_examples(&entries);
        assert!(out.contains("1. feat: alpha"));
        assert!(out.contains("   first body"));
        assert!(out.contains("2. fix: beta"));
        assert!(out.contains("   second body"));
    }

    #[test]
    fn test_format_historical_examples_quotes_delimiter_in_body() {
        // A body that contains our section header literally should still
        // render — the numbered-list prefix prevents the LLM from misreading it
        // as a new section header.
        let body_with_marker =
            "feat: edge case\n\n## Project commit-style references inside body".to_string();
        let out = format_historical_examples(&[body_with_marker]);
        assert!(out.contains("1. feat: edge case"));
        assert!(out.contains("## Project commit-style references inside body"));
        // Two occurrences total: header + the embedded one
        assert_eq!(out.matches("## Project commit-style references").count(), 2);
    }

    #[test]
    fn test_commit_prompt_scope_with_root_changes() {
        let ctx = CommitContext {
            files_changed: vec!["packages/core/src/lib.rs".into(), "README.md".into()],
            insertions: 3,
            deletions: 1,
            branch_name: None,
            custom_prompt: None,
            user_feedback: vec![],
            convention: None,
            scope_info: Some(ScopeInfo {
                workspace_types: vec!["pnpm".into()],
                packages: vec!["packages/core".into()],
                suggested_scope: Some("core".into()),
                has_root_changes: true,
            }),
            historical_examples: Vec::new(),
        };
        let (_, user) = build_commit_prompt_split("diff", &ctx, None, None);

        assert!(user.contains("root-level"));
    }
}
