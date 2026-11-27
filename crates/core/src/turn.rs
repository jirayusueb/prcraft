//! Whose turn a reviewed change is. Pure rules over a [`Snapshot`] the forge layer reads.

use serde::Serialize;

use crate::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
}

impl ReviewState {
    /// `You requested changes`, `You approved`, `You commented`.
    pub fn you(self) -> &'static str {
        match self {
            ReviewState::Approved => "You approved",
            ReviewState::ChangesRequested => "You requested changes",
            ReviewState::Commented => "You commented",
        }
    }
}

/// The user's latest review of a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MyReview {
    pub state: ReviewState,
    /// `None` only for a GitLab approval whose note is gone. Everything "after the review" then counts nothing.
    pub submitted_at: Option<DateTime<Utc>>,
    /// The commit the review was written on. GitLab records none, so its parser picks the last commit before the review.
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub author: String,
    pub at: DateTime<Utc>,
    pub body: String,
}

/// A review thread, oldest note first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Thread {
    pub resolved: bool,
    /// Who resolved it, when the forge says.
    pub resolved_by: Option<String>,
    /// When it was resolved, when the forge says (GitLab does; GitHub does not).
    pub resolved_at: Option<DateTime<Utc>>,
    /// The review its first note belongs to (GitHub). `None` where the forge has no review objects.
    pub review_id: Option<String>,
    pub notes: Vec<Note>,
}

/// What a forge says about one change, in forge-neutral words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub me: String,
    pub author: String,
    pub review: MyReview,
    /// The change's head commit now.
    pub head: String,
    /// Oldest first.
    pub commits: Vec<Commit>,
    pub threads: Vec<Thread>,
    /// Comments on the change as a whole, outside any thread.
    pub notes: Vec<Note>,
    /// The forge asks the user to review again.
    pub re_requested: bool,
    /// When that request was made, if the forge says. It tells one request from the next.
    pub re_requested_at: Option<DateTime<Utc>>,
    /// The user's approval stopped counting without the user taking it back.
    pub approval_dismissed: bool,
    /// The id of the review `review` describes (GitHub). Threads count for "all resolved" only when they belong to it.
    pub review_id: Option<String>,
    /// `commits` holds every commit of the change, so a missing review commit means it was rewritten away.
    pub commits_complete: bool,
    /// Why part of the change could not be read, when so. The turn is then a best guess.
    pub incomplete: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    NewCommits {
        count: u32,
    },
    /// `count` is what still waits for an answer; `last_reply` is the newest author note after my review, whatever
    /// I answered since, so answering some threads does not make the same replies look new.
    AuthorReplied {
        count: u32,
        last_reply: DateTime<Utc>,
    },
    ThreadsResolved {
        count: u32,
    },
    ReRequested {
        at: Option<DateTime<Utc>>,
    },
    Mentioned {
        count: u32,
    },
    ApprovalDismissed,
}

impl Reason {
    pub fn text(&self) -> String {
        let plural = |n: &u32, one: &str, many: &str| if *n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
        match self {
            Reason::NewCommits { count } => plural(count, "new commit", "new commits"),
            Reason::AuthorReplied { count, .. } => plural(count, "reply from the author", "replies from the author"),
            Reason::ThreadsResolved { count } => format!("{} resolved, none left open", plural(count, "thread of yours", "threads of yours")),
            Reason::ReRequested { .. } => "asked you to review again".into(),
            Reason::Mentioned { count } => plural(count, "mention of you", "mentions of you"),
            Reason::ApprovalDismissed => "your approval was dismissed".into(),
        }
    }

    /// Changes whenever the reason grows or happens again, so a notification about it is new again.
    pub fn key(&self) -> String {
        match self {
            Reason::NewCommits { count } => format!("commits{count}"),
            Reason::AuthorReplied { last_reply, .. } => format!("replies@{}", last_reply.timestamp()),
            Reason::ThreadsResolved { count } => format!("resolved{count}"),
            Reason::ReRequested { at } => format!("re_requested@{}", at.map_or(0, |at| at.timestamp())),
            Reason::Mentioned { count } => format!("mentions{count}"),
            Reason::ApprovalDismissed => "dismissed".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "turn", rename_all = "snake_case")]
pub enum Turn {
    BackToYou { reasons: Vec<Reason> },
    WithAuthor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assessment {
    pub review: MyReview,
    /// Commits pushed after the commit the review was written on.
    pub commits_since: u32,
    #[serde(flatten)]
    pub turn: Turn,
}

/// Decides whose turn it is. After an approval only a dismissal brings the change back; after a review that
/// does not approve, any of: new commits, an author reply on one of the user's threads, all those threads
/// resolved, a new review request, an @mention.
pub fn assess(s: &Snapshot) -> Assessment {
    let commits_since = commits_since(s);
    let turn = turn_of(s, commits_since);
    Assessment { review: s.review.clone(), commits_since, turn }
}

fn turn_of(s: &Snapshot, commits_since: u32) -> Turn {
    if s.review.state == ReviewState::Approved {
        return if s.approval_dismissed { Turn::BackToYou { reasons: vec![Reason::ApprovalDismissed] } } else { Turn::WithAuthor };
    }
    let after = |at: DateTime<Utc>| s.review.submitted_at.is_some_and(|since| at > since);
    let mine = |t: &&Thread| t.notes.first().is_some_and(|n| n.author == s.me);
    let own_threads: Vec<&Thread> = s.threads.iter().filter(mine).collect();
    let is_reply = |t: &Thread, n: &Note| mine(&t) && n.author == s.author;
    // An answer I have answered back is no longer waiting on me.
    let answers: Vec<&Note> = own_threads
        .iter()
        .flat_map(|t| {
            let mine_last = t.notes.iter().filter(|n| n.author == s.me).map(|n| n.at).max();
            t.notes.iter().skip(1).filter(move |n| n.author == s.author && after(n.at) && mine_last.is_none_or(|last| n.at > last))
        })
        .collect();
    let last_reply = own_threads.iter().flat_map(|t| t.notes.iter().skip(1)).filter(|n| n.author == s.author && after(n.at)).map(|n| n.at).max();
    let mentions = s
        .threads
        .iter()
        .flat_map(|t| t.notes.iter().map(move |n| (t, n)))
        .filter(|(t, n)| !is_reply(t, n))
        .map(|(_, n)| n)
        .chain(&s.notes)
        .filter(|n| n.author != s.me && after(n.at) && mentions(&n.body, &s.me))
        .count();
    // A thread of any review of mine that is still open keeps the change with the author. Only threads of the
    // review being answered can be news when resolved.
    let mut open = own_threads.iter().filter(|t| !t.resolved).count();
    let mut fresh = 0;
    let answered = |t: &&&Thread| t.resolved && (s.review_id.is_none() || t.review_id == s.review_id);
    for thread in own_threads.iter().filter(answered) {
        match standing(thread, &s.me, s.review.submitted_at) {
            Standing::Open => open += 1,
            Standing::Fresh => fresh += 1,
            Standing::Settled => {}
        }
    }
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let candidates = [
        (commits_since > 0).then_some(Reason::NewCommits { count: commits_since }),
        last_reply.filter(|_| !answers.is_empty()).map(|last_reply| Reason::AuthorReplied { count: count(answers.len()), last_reply }),
        (open == 0 && fresh > 0).then_some(Reason::ThreadsResolved { count: count(fresh) }),
        s.re_requested.then_some(Reason::ReRequested { at: s.re_requested_at }),
        (mentions > 0).then_some(Reason::Mentioned { count: count(mentions) }),
    ];
    let reasons: Vec<Reason> = candidates.into_iter().flatten().collect();
    if reasons.is_empty() { Turn::WithAuthor } else { Turn::BackToYou { reasons } }
}

enum Standing {
    Open,
    /// Resolved by someone else after the review.
    Fresh,
    /// Resolved by the user, or resolved before the review: nothing to tell.
    Settled,
}

fn standing(thread: &Thread, me: &str, reviewed_at: Option<DateTime<Utc>>) -> Standing {
    if !thread.resolved {
        return Standing::Open;
    }
    let by_me = thread.resolved_by.as_deref().is_some_and(|by| by.eq_ignore_ascii_case(me));
    let before_review = thread.resolved_at.is_some_and(|at| reviewed_at.is_none_or(|since| at <= since));
    if by_me || before_review { Standing::Settled } else { Standing::Fresh }
}

/// The commit the review was written on is not among the change's commits: it was force-pushed or rebased away.
pub fn rewritten(s: &Snapshot) -> bool {
    s.commits_complete && s.review.commit.as_ref().is_some_and(|sha| !s.commits.iter().any(|c| &c.sha == sha))
}

/// Commits after the one the review sat on. When that commit is gone (a force-push), commits dated after the review.
fn commits_since(s: &Snapshot) -> u32 {
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    if let Some(at) = s.review.commit.as_ref().and_then(|sha| s.commits.iter().position(|c| &c.sha == sha)) {
        return count(s.commits.len() - at - 1);
    }
    s.review.submitted_at.map_or(0, |since| count(s.commits.iter().filter(|c| c.at > since).count()))
}

/// Whether `text` mentions `@login`, not `@login-two` and not an address such as `a@login.dev`.
pub fn mentions(text: &str, login: &str) -> bool {
    if login.is_empty() {
        return false;
    }
    let (text, tag) = (text.to_lowercase(), format!("@{}", login.to_lowercase()));
    let in_login = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    text.match_indices(&tag).any(|(at, _)| {
        let before = text.get(..at).and_then(|s| s.chars().next_back());
        let after = text.get(at + tag.len()..).and_then(|s| s.chars().next());
        !before.is_some_and(in_login) && !after.is_some_and(in_login)
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 20, hour, 0, 0).single().expect("a valid time")
    }

    fn note(author: &str, hour: u32, body: &str) -> Note {
        Note { author: author.into(), at: at(hour), body: body.into() }
    }

    /// An invented change: `mo` reviewed at 10:00 on commit `a1`, `nina` is the author, nothing happened since.
    fn quiet(state: ReviewState) -> Snapshot {
        Snapshot {
            me: "mo".into(),
            author: "nina".into(),
            review: MyReview { state, submitted_at: Some(at(10)), commit: Some("a1".into()) },
            head: "a1".into(),
            commits: vec![Commit { sha: "a0".into(), at: at(8) }, Commit { sha: "a1".into(), at: at(9) }],
            threads: vec![Thread { notes: vec![note("mo", 10, "needs a lock")], ..Thread::default() }],
            notes: Vec::new(),
            re_requested: false,
            re_requested_at: None,
            approval_dismissed: false,
            review_id: None,
            commits_complete: true,
            incomplete: None,
        }
    }

    fn replied(count: u32, hour: u32) -> Reason {
        Reason::AuthorReplied { count, last_reply: at(hour) }
    }

    fn reasons(s: &Snapshot) -> Vec<Reason> {
        match assess(s).turn {
            Turn::BackToYou { reasons } => reasons,
            Turn::WithAuthor => Vec::new(),
        }
    }

    #[test]
    fn nothing_since_the_review_leaves_it_with_the_author() {
        let s = quiet(ReviewState::ChangesRequested);
        assert_eq!(assess(&s).turn, Turn::WithAuthor);
        assert_eq!(assess(&s).commits_since, 0);
    }

    #[test]
    fn new_commits_after_the_reviewed_one_bring_it_back() {
        let mut s = quiet(ReviewState::Commented);
        s.commits.push(Commit { sha: "b1".into(), at: at(11) });
        s.commits.push(Commit { sha: "b2".into(), at: at(12) });
        assert_eq!(reasons(&s), [Reason::NewCommits { count: 2 }]);
    }

    #[test]
    fn a_force_pushed_review_commit_counts_commits_dated_after_the_review() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.review.commit = Some("gone".into());
        s.commits = vec![Commit { sha: "c0".into(), at: at(9) }, Commit { sha: "c1".into(), at: at(11) }];
        assert_eq!(reasons(&s), [Reason::NewCommits { count: 1 }]);
    }

    #[test]
    fn an_author_reply_on_my_thread_brings_it_back_but_my_own_note_does_not() {
        let mut s = quiet(ReviewState::ChangesRequested);
        let thread = &mut s.threads[0];
        thread.notes.push(note("mo", 11, "also here"));
        assert_eq!(reasons(&s), Vec::new());
        s.threads[0].notes.push(note("nina", 12, "done"));
        assert_eq!(reasons(&s), [replied(1, 12)]);
    }

    #[test]
    fn a_reply_before_the_review_is_not_new() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.threads[0].notes.push(note("nina", 9, "answered earlier"));
        assert_eq!(reasons(&s), Vec::new());
    }

    #[test]
    fn my_own_later_reply_answers_the_authors_answer() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.threads[0].notes.push(note("nina", 11, "done"));
        s.threads[0].notes.push(note("mo", 12, "thanks"));
        assert_eq!(reasons(&s), Vec::new());
        s.threads[0].notes.push(note("nina", 13, "one more fix"));
        assert_eq!(reasons(&s), [replied(1, 13)], "only the answer after my last note counts");
    }

    #[test]
    fn an_open_thread_of_an_older_review_keeps_all_resolved_from_firing() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.review_id = Some("R2".into());
        s.threads[0].review_id = Some("R1".into());
        s.threads.push(Thread {
            resolved: true,
            resolved_by: Some("nina".into()),
            review_id: Some("R2".into()),
            notes: vec![note("mo", 10, "newer")],
            ..Thread::default()
        });
        assert_eq!(reasons(&s), Vec::new(), "R1's thread is still open");
        s.threads[0].resolved = true;
        assert_eq!(reasons(&s), [Reason::ThreadsResolved { count: 1 }], "the resolved R1 thread is not news, the R2 one is");
    }

    #[test]
    fn all_my_threads_resolved_brings_it_back_and_one_open_thread_does_not() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.threads.push(Thread { resolved: true, notes: vec![note("mo", 10, "second")], ..Thread::default() });
        assert_eq!(reasons(&s), Vec::new());
        s.threads[0].resolved = true;
        assert_eq!(reasons(&s), [Reason::ThreadsResolved { count: 2 }]);
    }

    #[test]
    fn someone_elses_resolved_thread_is_not_mine_to_see() {
        let mut s = quiet(ReviewState::Commented);
        s.threads = vec![Thread { resolved: true, notes: vec![note("jp", 9, "other reviewer")], ..Thread::default() }];
        assert_eq!(reasons(&s), Vec::new(), "no thread of mine, so none to resolve");
    }

    #[test]
    fn a_new_review_request_brings_it_back() {
        let mut s = quiet(ReviewState::Commented);
        s.re_requested = true;
        s.re_requested_at = Some(at(11));
        assert_eq!(reasons(&s), [Reason::ReRequested { at: Some(at(11)) }]);
    }

    #[test]
    fn a_mention_after_the_review_brings_it_back_but_not_before_it_nor_from_me() {
        let mut s = quiet(ReviewState::Commented);
        s.notes = vec![note("nina", 9, "@mo early"), note("mo", 12, "@mo talking to myself"), note("nina", 11, "@mo could you look again?")];
        assert_eq!(reasons(&s), [Reason::Mentioned { count: 1 }]);
    }

    #[test]
    fn a_reply_that_mentions_me_counts_once() {
        let mut s = quiet(ReviewState::Commented);
        s.threads[0].notes.push(note("nina", 11, "@mo fixed"));
        assert_eq!(reasons(&s), [replied(1, 11)]);
    }

    #[test]
    fn a_dismissed_approval_brings_it_back() {
        let mut s = quiet(ReviewState::Approved);
        s.approval_dismissed = true;
        assert_eq!(reasons(&s), [Reason::ApprovalDismissed]);
    }

    #[test]
    fn an_approval_stays_with_the_author_whatever_happens_after() {
        let mut s = quiet(ReviewState::Approved);
        s.commits.push(Commit { sha: "b1".into(), at: at(11) });
        s.threads[0].notes.push(note("nina", 12, "done"));
        s.re_requested = true;
        s.notes = vec![note("nina", 12, "@mo thanks")];
        assert_eq!(assess(&s).turn, Turn::WithAuthor);
    }

    #[test]
    fn reasons_come_in_a_fixed_order() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.commits.push(Commit { sha: "b1".into(), at: at(11) });
        s.threads[0].notes.push(note("nina", 11, "done"));
        s.threads[0].resolved = true;
        s.re_requested = true;
        s.notes = vec![note("lea", 12, "cc @mo")];
        let found = reasons(&s);
        assert!(
            matches!(
                found.as_slice(),
                [
                    Reason::NewCommits { .. },
                    Reason::AuthorReplied { .. },
                    Reason::ThreadsResolved { .. },
                    Reason::ReRequested { .. },
                    Reason::Mentioned { .. }
                ]
            ),
            "{found:?}"
        );
    }

    #[test]
    fn a_new_episode_of_the_same_kind_has_a_different_key() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.threads[0].notes.push(note("nina", 11, "done"));
        let first = reasons(&s);
        // I answer, then the author replies again: one reply is waiting again, as the first time.
        s.threads[0].notes.push(note("mo", 12, "thanks"));
        s.threads[0].notes.push(note("nina", 13, "and one more"));
        let second = reasons(&s);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].key(), second[0].key());
        let (early, late) = (Reason::ReRequested { at: Some(at(11)) }, Reason::ReRequested { at: Some(at(14)) });
        assert_ne!(early.key(), late.key(), "a second request is news again");
    }

    #[test]
    fn answering_replies_one_thread_at_a_time_keeps_the_event_and_a_new_author_note_changes_it() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.threads.push(Thread { notes: vec![note("mo", 10, "second thread")], ..Thread::default() });
        s.threads[0].notes.push(note("nina", 11, "first done"));
        s.threads[1].notes.push(note("nina", 12, "second done"));
        let both = reasons(&s);
        s.threads[0].notes.push(note("mo", 13, "ok"));
        let one_left = reasons(&s);
        assert_eq!((both.len(), one_left.len()), (1, 1));
        assert_eq!(both[0].key(), one_left[0].key(), "my partial answer is not a new event");
        s.threads[1].notes.push(note("mo", 14, "ok"));
        assert!(reasons(&s).is_empty());
        s.threads[0].notes.push(note("nina", 15, "actually, one more"));
        let again = reasons(&s);
        assert_eq!(again.len(), 1);
        assert_ne!(again[0].key(), both[0].key());
    }

    #[test]
    fn a_login_inside_another_name_or_an_address_is_not_a_mention() {
        assert!(mentions("ping @Mo, please", "mo"));
        assert!(mentions("@mo", "mo"));
        assert!(!mentions("ping @mom", "mo"));
        assert!(!mentions("ping @mo-two", "mo"));
        assert!(!mentions("mail a@mo.dev", "mo"));
        assert!(!mentions("anything", ""));
    }

    #[test]
    fn a_thread_i_resolved_myself_is_not_news() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.threads[0].resolved = true;
        s.threads[0].resolved_by = Some("MO".into());
        assert_eq!(reasons(&s), Vec::new());
        s.threads[0].resolved_by = Some("nina".into());
        assert_eq!(reasons(&s), [Reason::ThreadsResolved { count: 1 }]);
    }

    #[test]
    fn a_thread_resolved_before_my_review_is_not_news() {
        let mut s = quiet(ReviewState::ChangesRequested);
        s.threads[0].resolved = true;
        s.threads[0].resolved_by = Some("nina".into());
        s.threads[0].resolved_at = Some(at(9));
        assert_eq!(reasons(&s), Vec::new(), "resolved before the 10:00 review");
        s.threads[0].resolved_at = Some(at(11));
        assert_eq!(reasons(&s), [Reason::ThreadsResolved { count: 1 }]);
    }

    #[test]
    fn threads_of_an_older_review_do_not_keep_all_resolved_alive_after_a_newer_review() {
        // The latest review R2 opened no thread; the resolved thread belongs to the earlier review R1.
        let mut s = quiet(ReviewState::Commented);
        s.review_id = Some("R2".into());
        s.threads[0].review_id = Some("R1".into());
        s.threads[0].resolved = true;
        s.threads[0].resolved_by = Some("nina".into());
        assert_eq!(reasons(&s), Vec::new());
        s.threads[0].review_id = Some("R2".into());
        assert_eq!(reasons(&s), [Reason::ThreadsResolved { count: 1 }]);
    }

    #[test]
    fn a_review_commit_missing_from_the_complete_commit_list_was_rewritten() {
        let mut s = quiet(ReviewState::ChangesRequested);
        assert!(!rewritten(&s));
        s.review.commit = Some("gone".into());
        assert!(rewritten(&s));
        s.commits_complete = false;
        assert!(!rewritten(&s), "a partial list proves nothing");
        s.commits_complete = true;
        s.review.commit = None;
        assert!(!rewritten(&s), "no recorded commit, nothing to rewrite");
    }
}
