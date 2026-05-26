//! Richer commit metadata for prompt-style history injection.
//!
//! [`HistoricalCommit`] carries the full message body in addition to the
//! subject, used by [`crate::llm::history_sampler`] to construct style
//! reference examples for the LLM. The plain [`super::CommitInfo`] keeps the
//! subject-only contract used by `stats` so memory pressure on large repos
//! is not regressed.
//!
//! The git-side extraction lives in [`super::repository::GitRepository`]
//! (see `get_commit_history_full`, added in Iteration E). This module only
//! defines the data type and pure helpers over its message text.

use chrono::{DateTime, Local};

/// A single past commit reified with enough information for the sampler to
/// score it, bucket it by author, and format it as a prompt reference.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoricalCommit {
    /// Commit SHA hex string.
    pub hash: String,
    /// Number of parent commits (>1 means merge commit).
    pub parent_count: usize,
    /// Commit author name.
    pub author_name: String,
    /// Commit author email — used as the bucketing key.
    pub author_email: String,
    /// Commit timestamp in local timezone — used for recency scoring.
    pub timestamp: DateTime<Local>,
    /// First line of the commit message.
    pub subject: String,
    /// Remainder of the commit message (everything after the first blank line),
    /// trimmed of trailing whitespace. Empty when the commit only has a subject.
    pub body: String,
}

/// Splits a raw git commit message into `(subject, body)`.
///
/// The split point is the first blank line (`"\n\n"`). When no blank line is
/// present the entire message's first line becomes the subject and the body
/// is empty — matching the conventional one-line-summary contract.
///
/// CRLF line endings (`"\r\n"`) are normalized to `"\n"` before splitting so
/// Windows-origin commits split correctly. Trailing whitespace is trimmed
/// from both parts.
#[allow(dead_code)] // consumed by GitRepository::get_commit_history_full in Iteration E
pub(crate) fn split_message(raw: &str) -> (String, String) {
    // Normalize CRLF → LF so the blank-line separator is detected uniformly
    // regardless of the committer's platform.
    let normalized = if raw.contains('\r') {
        raw.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        raw.to_string()
    };
    let trimmed = normalized.trim_end();
    if let Some(blank_idx) = trimmed.find("\n\n") {
        let subject = trimmed[..blank_idx].to_string();
        let body = trimmed[blank_idx + 2..].trim_end().to_string();
        return (subject, body);
    }
    let first_line = trimmed.split('\n').next().unwrap_or("").to_string();
    (first_line, String::new())
}

impl HistoricalCommit {
    /// Renders this commit as a single prompt-ready string.
    ///
    /// - `include_body = false` returns only the subject.
    /// - `include_body = true` returns `subject` if [`body`] is empty, or
    ///   `"{subject}\n\n{body}"` otherwise.
    ///
    /// No trailing newline is appended so the caller can choose its own
    /// separator when joining multiple entries.
    #[allow(dead_code)] // consumed by enforce_char_budget in Iteration D
    pub fn format_for_prompt(&self, include_body: bool) -> String {
        if !include_body || self.body.is_empty() {
            self.subject.clone()
        } else {
            format!("{}\n\n{}", self.subject, self.body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn mk(subject: &str, body: &str) -> HistoricalCommit {
        HistoricalCommit {
            hash: "h".to_string(),
            parent_count: 1,
            author_name: "n".to_string(),
            author_email: "e".to_string(),
            timestamp: Local.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            subject: subject.to_string(),
            body: body.to_string(),
        }
    }

    #[test]
    fn test_format_commit_subject_only_when_body_empty() {
        let c = mk("feat: foo", "");
        assert_eq!(c.format_for_prompt(true), "feat: foo");
        assert!(!c.format_for_prompt(true).ends_with('\n'));
    }

    #[test]
    fn test_format_commit_subject_and_body_with_blank_line() {
        let c = mk("feat: foo", "body line 1\nbody line 2");
        assert_eq!(
            c.format_for_prompt(true),
            "feat: foo\n\nbody line 1\nbody line 2"
        );
    }

    #[test]
    fn test_format_commit_include_body_false_strips_body() {
        let c = mk("feat: foo", "details here");
        assert_eq!(c.format_for_prompt(false), "feat: foo");
    }

    #[test]
    fn test_historical_commit_split_subject_body() {
        let (subject, body) = split_message("feat: foo\n\nbody line 1\nbody line 2\n");
        assert_eq!(subject, "feat: foo");
        assert_eq!(body, "body line 1\nbody line 2");
    }

    #[test]
    fn test_historical_commit_split_subject_only() {
        let (subject, body) = split_message("chore: bump version");
        assert_eq!(subject, "chore: bump version");
        assert_eq!(body, "");
    }

    #[test]
    fn test_historical_commit_split_strips_trailing_whitespace() {
        let (subject, body) = split_message("fix: x\n\nDetails.\n\n\n");
        assert_eq!(subject, "fix: x");
        assert_eq!(body, "Details.");
    }

    #[test]
    fn test_historical_commit_split_handles_crlf_separator() {
        // Windows-origin commit uses CRLF for both subject/body boundary
        // and intra-body line breaks.
        let (subject, body) = split_message("feat: foo\r\n\r\nbody line 1\r\nbody line 2\r\n");
        assert_eq!(subject, "feat: foo");
        assert_eq!(body, "body line 1\nbody line 2");
    }

    #[test]
    fn test_historical_commit_split_handles_mixed_line_endings() {
        // Some clients write subject with LF and body with CRLF (or vice versa).
        let (subject, body) = split_message("feat: foo\n\r\nbody\r\n");
        assert_eq!(subject, "feat: foo");
        assert_eq!(body, "body");
    }
}
