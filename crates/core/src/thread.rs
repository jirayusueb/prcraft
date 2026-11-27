//! Review threads as the forge keeps them: a conversation on a line, answered and resolved on the forge itself, unlike a
//! [`crate::review::Draft`], which stays on this Mac until the review posts. No I/O.

use serde::{Deserialize, Serialize};

use crate::review::LineRef;
use crate::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadComment {
    pub author: String,
    pub at: Option<DateTime<Utc>>,
    pub body: String,
}

/// What a thread was first written on, for the "Show original" view of an outdated thread.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Original {
    /// The commit it was written on.
    pub commit: Option<String>,
    /// Its line on that commit.
    pub line: Option<u32>,
    /// The code the forge kept with the comment: the last lines of the hunk it was written on.
    pub code: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewThread {
    /// The forge's id for the thread: a GraphQL node id on GitHub, a discussion id on GitLab. `thread.reply` and
    /// `thread.resolve` take it back.
    pub id: String,
    pub path: String,
    /// Its line in the diff being reviewed. `None` when the lines moved or went away: the thread is outdated.
    pub line: Option<LineRef>,
    pub original: Original,
    pub resolved: bool,
    pub resolved_by: Option<String>,
    pub outdated: bool,
    /// The forge lets this user resolve it (a plain note on GitLab cannot be resolved).
    pub can_resolve: bool,
    pub can_reply: bool,
    pub comments: Vec<ThreadComment>,
    /// The forge holds more comments than `comments` lists.
    pub comments_total: u32,
}

impl ReviewThread {
    /// A thread sits under a line of the current diff only when it is not outdated and the forge placed it.
    pub fn placed(&self) -> bool {
        !self.outdated && self.line.is_some()
    }

    /// `2 comments`, or `1 comment`; with the real total when the list is partial.
    pub fn comment_count(&self) -> String {
        let total = self.comments_total.max(self.comments.len() as u32);
        if total == 1 { "1 comment".into() } else { format!("{total} comments") }
    }

    /// The first comment's first line, for a thread folded to one line.
    pub fn first_line(&self) -> &str {
        self.comments.first().and_then(|c| c.body.lines().next()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(outdated: bool, line: Option<u32>) -> ReviewThread {
        ReviewThread {
            id: "t".into(),
            path: "a.go".into(),
            line: line.map(|n| LineRef { old: None, new: Some(n) }),
            original: Original::default(),
            resolved: false,
            resolved_by: None,
            outdated,
            can_resolve: true,
            can_reply: true,
            comments: vec![ThreadComment { author: "ada".into(), at: None, body: "First line\nsecond".into() }],
            comments_total: 0,
        }
    }

    #[test]
    fn an_outdated_thread_or_one_without_a_line_is_never_placed_under_a_line() {
        assert!(thread(false, Some(3)).placed());
        assert!(!thread(true, Some(3)).placed(), "the forge says the code moved");
        assert!(!thread(false, None).placed(), "no line to sit under");
    }

    #[test]
    fn a_partial_comment_list_reports_the_forge_total() {
        let mut t = thread(false, Some(1));
        assert_eq!(t.comment_count(), "1 comment");
        t.comments_total = 130;
        assert_eq!(t.comment_count(), "130 comments");
        assert_eq!(t.first_line(), "First line");
    }
}
