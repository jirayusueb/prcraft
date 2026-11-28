//! A review being written on this Mac: line comments, a verdict and a summary, kept until they post.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::{Deserialize, Serialize};

use crate::extra::Extra;
use crate::{ChangeRef, Forge};

/// One diff line by its numbers on each side. A context line has both, an added line only `new`,
/// a removed line only `old`. GitLab needs both on a context line; GitHub needs one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRef {
    pub old: Option<u32>,
    pub new: Option<u32>,
}

impl LineRef {
    /// The number shown to the reader: the new side when there is one.
    pub fn shown(self) -> u32 {
        self.new.or(self.old).unwrap_or(0)
    }

    /// Whether this names the drawn line: each side it gives is the line's. A line a whitespace-only change folded into an
    /// unchanged line has both numbers, so a comment written on its added or removed half still finds it.
    pub fn on(self, line: &crate::diff::DiffLine) -> bool {
        (self.old.is_some() || self.new.is_some()) && (self.old.is_none() || self.old == line.old) && (self.new.is_none() || self.new == line.new)
    }
}

/// A line comment that has not posted yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: u64,
    pub path: String,
    /// The path before a rename, so GitLab can anchor the comment.
    pub old_path: Option<String>,
    /// First line of a multi-line comment; `None` for one line.
    pub start: Option<LineRef>,
    pub end: LineRef,
    pub body: String,
    /// Written on the rendered view of a file whose view has no lines of its own (an SVG); it still posts on `end`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_rendered: bool,
}

impl Draft {
    /// `retry.go:42` or `retry.go:42–44`.
    pub fn location(&self) -> String {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        match self.start {
            Some(start) if start != self.end => format!("{name}:{}–{}", start.shown(), self.end.shown()),
            _ => format!("{name}:{}", self.end.shown()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    #[default]
    Comment,
    Approve,
    RequestChanges,
}

impl Verdict {
    /// The verdicts a forge accepts. GitLab has no "request changes" on a merge request review.
    pub fn offered(forge: Forge) -> &'static [Verdict] {
        match forge {
            Forge::GitHub => &[Verdict::Comment, Verdict::Approve, Verdict::RequestChanges],
            Forge::GitLab => &[Verdict::Comment, Verdict::Approve],
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Verdict::Comment => "Comment",
            Verdict::Approve => "Approve",
            Verdict::RequestChanges => "Request changes",
        }
    }
}

/// The commit a review's line numbers belong to. Posting against a later commit would land
/// comments on the wrong lines, so drafts post against this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub head_sha: String,
    /// GitLab's other two diff refs; `None` on GitHub.
    pub base_sha: Option<String>,
    pub start_sha: Option<String>,
}

impl Anchor {
    /// The ids of the drafts written on another commit than `self` whose lines the diff `files` of `self` no longer has. A draft on an
    /// unchanged head is never outdated. shortcut: a line that kept its numbers but changed its text still counts as in place, because
    /// a draft keeps no text of its line; upgrade when it does.
    pub fn outdated_drafts(&self, local: &LocalReview, files: &[crate::diff::FileDiff]) -> Vec<u64> {
        if local.anchor.as_ref().is_none_or(|written| written == self) {
            return Vec::new();
        }
        let drawn =
            |path: &str, line: LineRef| files.iter().filter(|f| f.path == path).flat_map(|f| &f.hunks).flat_map(|h| &h.lines).any(|l| line.on(l));
        let in_place = |d: &Draft| drawn(&d.path, d.end) && d.start.is_none_or(|start| drawn(&d.path, start));
        local.drafts.iter().filter(|d| !in_place(d)).map(|d| d.id).collect()
    }
}

/// A verdict a newer build wrote reads as Comment, which never approves or blocks, so the drafts beside it still load.
fn lenient_verdict<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Verdict, D::Error> {
    let word = String::deserialize(d)?;
    Ok(serde_json::from_value(serde_json::Value::String(word)).unwrap_or_default())
}

/// Everything the reviewer wrote for one change, plus the files they marked viewed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalReview {
    pub drafts: Vec<Draft>,
    #[serde(deserialize_with = "lenient_verdict")]
    pub verdict: Verdict,
    /// The general comment that posts with the verdict.
    pub body: String,
    /// Viewed file path to the [`crate::diff::FileDiff::hash`] it had when marked.
    pub viewed: BTreeMap<String, u64>,
    pub next_id: u64,
    /// The commit the drafts were written on; set by the first draft, cleared when none are left.
    pub anchor: Option<Anchor>,
    /// Set when a post to the forge ran out of time: the forge may have taken it and its answer was lost. The drafts stay, and the
    /// next Submit asks first, because posting again could post twice. Says what happened, in words.
    pub post_unknown: Option<String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl LocalReview {
    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty() && self.body.trim().is_empty() && self.verdict == Verdict::Comment
    }

    /// The review as Markdown, for Copy as Markdown when the forge refuses it.
    pub fn markdown(&self, change: &ChangeRef, title: &str) -> String {
        let heading = [change.reference().as_str(), title.trim()].join(" ");
        let mut out = format!("## Review of {}\n\nVerdict: {}\n", heading.trim_end(), self.verdict.word());
        if !self.body.trim().is_empty() {
            let _ = write!(out, "\n{}\n", self.body.trim());
        }
        for draft in &self.drafts {
            let _ = write!(out, "\n**{}** (`{}`)\n\n{}\n", draft.location(), draft.path, draft.body.trim());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(sha: &str) -> Anchor {
        Anchor { head_sha: sha.into(), base_sha: None, start_sha: None }
    }

    #[test]
    fn a_draft_is_outdated_only_on_a_moved_head_and_a_line_the_diff_no_longer_has() {
        let files = crate::diff::parse_unified("diff --git a/f.go b/f.go\n--- a/f.go\n+++ b/f.go\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n");
        let draft = |id, line| Draft {
            id,
            path: "f.go".into(),
            old_path: None,
            start: None,
            end: LineRef { old: None, new: Some(line) },
            body: String::new(),
            on_rendered: false,
        };
        let local = |head| LocalReview { drafts: vec![draft(1, 2), draft(2, 9)], anchor: Some(at(head)), ..LocalReview::default() };
        assert!(at("b").outdated_drafts(&local("b"), &files).is_empty(), "same head: nothing moved");
        assert_eq!(at("b").outdated_drafts(&local("a"), &files), [2], "new head: line 2 is still drawn, line 9 is gone");
        assert!(at("b").outdated_drafts(&LocalReview::default(), &files).is_empty(), "no drafts");
    }

    #[test]
    fn markdown_keeps_every_draft_and_the_verdict() {
        let line = |n| LineRef { old: None, new: Some(n) };
        let review = LocalReview {
            drafts: vec![
                Draft {
                    id: 0,
                    path: "billing/invoicer.go".into(),
                    old_path: None,
                    start: Some(line(42)),
                    end: line(44),
                    body: "Lock first.".into(),
                    on_rendered: false,
                },
                Draft {
                    id: 1,
                    path: "db/0087.sql".into(),
                    old_path: None,
                    start: None,
                    end: line(3),
                    body: "Add a note.".into(),
                    on_rendered: false,
                },
            ],
            verdict: Verdict::RequestChanges,
            body: "Two things.".into(),
            ..LocalReview::default()
        };
        let change = ChangeRef { forge: Forge::GitLab, repo: "acme/billing".into(), number: 1293, host: None };
        let text = review.markdown(&change, "Finalize invoices");
        for needle in ["Request changes", "Two things.", "invoicer.go:42–44", "Lock first.", "0087.sql:3", "Add a note."] {
            assert!(text.contains(needle), "{needle} missing from:\n{text}");
        }
    }

    #[test]
    fn a_verdict_from_a_newer_build_reads_as_comment_and_keeps_the_drafts() {
        let file = r#"{"verdict": "something_new", "drafts": [{"id": 0, "path": "a.rs", "end": {"old": null, "new": 1}, "body": "why", "on_rendered": false}]}"#;
        let review: LocalReview = serde_json::from_str(file).expect("loads");
        assert_eq!(review.verdict, Verdict::Comment);
        assert_eq!(review.drafts.len(), 1);
    }
}
