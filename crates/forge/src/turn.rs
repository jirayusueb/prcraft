//! Reads what "back to you" needs: the changes the user reviewed and what happened on them since, and the
//! requests that hand a review to someone else. Everything here reads, except [`hand_off`].

use std::thread;

use pr_craft_core::turn::{Commit, MyReview, Note, ReviewState, Snapshot, Thread, mentions};
use pr_craft_core::{Change, ChangeRef, DateTime, Forge, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::review::project_path;
use crate::stack::{Compare, compare_diff};
use crate::{PAGE, from_json, parse_github, run_on, run_with_input_on, signed_in_hosts};

/// A change the user reviewed, with what the forge says happened on it since.
#[derive(Debug, Clone)]
pub struct Reviewed {
    pub change: Change,
    /// `Ok(None)`: the forge lists the change but holds no review of the user's on it.
    pub snapshot: Result<Option<Snapshot>, String>,
}

/// What [`reviewed`] found.
#[derive(Debug, Clone)]
pub struct ReviewedList {
    pub items: Vec<Reviewed>,
    /// The forge returned a full page ([`PAGE`]), so older reviews may be missing.
    pub truncated: bool,
}

/// One person as either forge names them.
#[derive(Deserialize)]
struct Person {
    #[serde(alias = "username")]
    login: String,
}

/// The login the CLI acts as on `host` (GitHub reads only github.com). Blocks on the CLI.
pub fn my_login(forge: Forge, host: &str) -> Result<String, String> {
    let me: Person = from_json(&run_on(host, forge.cli(), &["api", "user"])?)?;
    Ok(me.login)
}

/// The most search pages of reviewed changes read per host for Back to you. Every candidate costs one snapshot read,
/// so this stays small; a host that fills them is reported as truncated.
const REVIEWED_PAGES: usize = 3;

/// Open changes the user reviewed that someone else wrote, newest update first, at most [`REVIEWED_PAGES`] pages of
/// [`PAGE`] per host, each with its [`Snapshot`]. Every host the CLI is signed in to is read, as the login it has
/// there. A change whose lookup failed carries its error, so the caller never counts it as quiet. Blocks on the CLI;
/// call it off the UI thread.
pub fn reviewed(forge: Forge) -> Result<ReviewedList, String> {
    let mut truncated = false;
    let mut candidates: Vec<(Change, String)> = Vec::new();
    for host in signed_in_hosts(forge)? {
        let me = my_login(forge, &host).map_err(|e| format!("{host}: {e}"))?;
        let (found, cut) = match forge {
            Forge::GitHub => {
                let (fields, limit) = ("number,title,repository,author,url,updatedAt,isDraft", format!("--limit={}", PAGE * REVIEWED_PAGES));
                let args = ["search", "prs", "--reviewed-by=@me", "--state=open", "--sort=updated", &limit, "--json", fields];
                let found = run_on(&host, "gh", &args).and_then(|bytes| parse_github(&bytes, &host)).map_err(|e| format!("{host}: {e}"))?;
                let cut = found.len() >= PAGE * REVIEWED_PAGES;
                (found, cut)
            }
            Forge::GitLab => {
                let base = format!("merge_requests?state=opened&scope=all&order_by=updated_at&reviewer_username={me}&");
                let (mrs, cut) = pages_up_to::<crate::GitLabMr>(&host, &base, REVIEWED_PAGES).map_err(|e| format!("{host}: {e}"))?;
                (mrs.into_iter().map(|mr| crate::gitlab_change(mr, &host)).collect(), cut)
            }
        };
        truncated |= cut;
        candidates.extend(found.into_iter().map(|change| (change, me.clone())));
    }
    candidates.sort_by_key(|(change, _)| std::cmp::Reverse(change.updated_at));
    candidates.retain(|(change, me)| !change.author.eq_ignore_ascii_case(me));
    let mut found = Vec::new();
    for batch in candidates.chunks(8) {
        let snapshots: Vec<_> = thread::scope(|scope| {
            let jobs: Vec<_> = batch.iter().map(|(change, me)| scope.spawn(move || snapshot(&change.change_ref(), me))).collect();
            jobs.into_iter().map(|job| job.join().unwrap_or_else(|_| Err("the lookup stopped".into()))).collect()
        });
        found.extend(batch.iter().map(|(change, _)| change.clone()).zip(snapshots).map(|(change, snapshot)| Reviewed { change, snapshot }));
    }
    Ok(ReviewedList { items: found, truncated })
}

/// What happened on `change` around the user's latest review of it. `None` when the user has no review there.
/// Blocks on the CLI.
pub fn snapshot(change: &ChangeRef, me: &str) -> Result<Option<Snapshot>, String> {
    match change.forge {
        Forge::GitHub => {
            let (owner, name) = change.repo.split_once('/').ok_or_else(|| format!("{} is not `owner/name`", change.repo))?;
            let fields = [
                ("-f", format!("query={GITHUB_QUERY}")),
                ("-f", format!("owner={owner}")),
                ("-f", format!("name={name}")),
                ("-F", format!("number={}", change.number)),
                ("-f", format!("me={me}")),
            ];
            let mut args = vec!["api", "graphql"];
            for (flag, value) in &fields {
                args.extend([*flag, value.as_str()]);
            }
            github_snapshot(&run_on(change.host_name(), "gh", &args)?, me)
        }
        Forge::GitLab => Ok(gitlab_snapshot(me, gitlab_read(change, me)?)),
    }
}

/// Everything `snapshot` needs of one pull request. The user's reviews are asked for by author and without pending
/// ones. Threads, whole-change comments and commits are the newest of each; a thread is its first comment and its
/// newest 20. Whatever does not fit is reported (`pageInfo`, `totalCount`), so the answer says when it is partial.
pub const GITHUB_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $me: String!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      author { login }
      headRefOid
      reviews(last: 100, author: $me, states: [APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) {
        pageInfo { hasPreviousPage }
        nodes {
          id state submittedAt body commit { oid }
          comments(first: 30) { totalCount nodes { replyTo { id } } }
        }
      }
      commits(last: 100) { pageInfo { hasPreviousPage } nodes { commit { oid committedDate } } }
      reviewThreads(last: 100) {
        pageInfo { hasPreviousPage }
        nodes {
          isResolved
          resolvedBy { login }
          opening: comments(first: 1) { nodes { ...ThreadComment } }
          recent: comments(last: 20) { totalCount nodes { ...ThreadComment } }
        }
      }
      reviewRequests(first: 50) { nodes { requestedReviewer { ... on User { login } } } }
      comments(last: 50) { nodes { author { login } createdAt body } }
      timelineItems(last: 50, itemTypes: [REVIEW_DISMISSED_EVENT]) {
        nodes { ... on ReviewDismissedEvent { previousReviewState review { id } } }
      }
      requests: timelineItems(last: 20, itemTypes: [REVIEW_REQUESTED_EVENT]) {
        nodes { ... on ReviewRequestedEvent { createdAt requestedReviewer { ... on User { login } } } }
      }
    }
  }
}
fragment ThreadComment on PullRequestReviewComment { id author { login } createdAt body pullRequestReview { id } }";

#[derive(Deserialize)]
struct GqlAnswer {
    data: Option<GqlData>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct GqlData {
    repository: Option<GqlRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRepository {
    pull_request: Option<GqlPull>,
}

/// A GraphQL connection. A node is `null` when the forge hides it from the user.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Nodes<T> {
    nodes: Vec<Option<T>>,
    page_info: Option<GqlPageInfo>,
    total_count: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPageInfo {
    has_previous_page: bool,
}

impl<T> Nodes<T> {
    fn items(self) -> impl Iterator<Item = T> {
        self.nodes.into_iter().flatten()
    }

    /// An earlier page exists that was not read (the connection was asked for its last nodes).
    fn has_earlier(&self) -> bool {
        self.page_info.as_ref().is_some_and(|p| p.has_previous_page)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPull {
    author: Option<GqlLogin>,
    head_ref_oid: String,
    reviews: Nodes<GqlReview>,
    commits: Nodes<GqlCommitNode>,
    review_threads: Nodes<GqlThread>,
    review_requests: Nodes<GqlRequest>,
    comments: Nodes<GqlComment>,
    timeline_items: Nodes<GqlDismissal>,
    /// Review requests, newest last; a request of mine says when I was asked.
    requests: Nodes<GqlRequestEvent>,
}

#[derive(Deserialize)]
struct GqlLogin {
    /// Absent on a team or a bot in a review request.
    login: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlReview {
    id: String,
    state: String,
    submitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    body: String,
    commit: Option<GqlOid>,
    comments: Option<Nodes<GqlReviewComment>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlReviewComment {
    /// Set on a comment that answers a thread someone else opened.
    reply_to: Option<GqlId>,
}

#[derive(Deserialize)]
struct GqlOid {
    oid: String,
}

#[derive(Deserialize)]
struct GqlCommitNode {
    commit: GqlCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlCommit {
    oid: String,
    committed_date: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThread {
    is_resolved: bool,
    resolved_by: Option<GqlLogin>,
    /// The thread's first comment, and its newest ones: the middle of a long thread is not read.
    opening: Nodes<GqlComment>,
    recent: Nodes<GqlComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlComment {
    id: Option<String>,
    author: Option<GqlLogin>,
    created_at: DateTime<Utc>,
    body: String,
    /// The review this comment was written in; set on review comments only.
    pull_request_review: Option<GqlId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRequest {
    requested_reviewer: Option<GqlLogin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlDismissal {
    previous_review_state: Option<String>,
    review: Option<GqlId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRequestEvent {
    created_at: Option<DateTime<Utc>>,
    requested_reviewer: Option<GqlLogin>,
}

#[derive(Deserialize)]
struct GqlId {
    id: String,
}

impl GqlComment {
    fn note(self) -> Note {
        Note { author: self.author.and_then(|a| a.login).unwrap_or_else(|| "ghost".into()), at: self.created_at, body: self.body }
    }
}

/// The review state of the latest review, and whether it is an approval the forge dismissed. GitHub keeps no
/// original state on a dismissed review, so the dismissal event says what it was.
fn github_state(latest: &GqlReview, dismissals: &[GqlDismissal]) -> Option<(ReviewState, bool)> {
    match latest.state.as_str() {
        "APPROVED" => Some((ReviewState::Approved, false)),
        "CHANGES_REQUESTED" => Some((ReviewState::ChangesRequested, false)),
        "COMMENTED" => Some((ReviewState::Commented, false)),
        "DISMISSED" => {
            let before = dismissals.iter().find(|d| d.review.as_ref().is_some_and(|r| r.id == latest.id));
            Some(match before.and_then(|d| d.previous_review_state.as_deref()) {
                Some("APPROVED") => (ReviewState::Approved, true),
                Some("CHANGES_REQUESTED") => (ReviewState::ChangesRequested, false),
                _ => (ReviewState::Commented, false),
            })
        }
        _ => None,
    }
}

/// A comment-only review that wrote no summary and only answered threads others opened. It is a reply, not a
/// review: it must not move "my latest review" (the commit to compare from, and what counts as new).
fn github_reply_only(review: &GqlReview) -> bool {
    let Some(comments) = &review.comments else { return false };
    let read = comments.nodes.len();
    review.state == "COMMENTED"
        && review.body.trim().is_empty()
        && read > 0
        && comments.total_count.is_none_or(|total| usize::try_from(total).is_ok_and(|total| total <= read))
        && comments.nodes.iter().all(|c| c.as_ref().is_some_and(|c| c.reply_to.is_some()))
}

/// A thread as the snapshot holds it, and whether comments between its first and its newest were not read.
fn github_thread(thread: GqlThread) -> (Thread, bool) {
    let total = thread.recent.total_count.and_then(|t| usize::try_from(t).ok()).unwrap_or(0);
    let opening: Vec<GqlComment> = thread.opening.items().collect();
    let review_id = opening.first().and_then(|c| c.pull_request_review.as_ref()).map(|r| r.id.clone());
    let opened: Vec<String> = opening.iter().filter_map(|c| c.id.clone()).collect();
    let mut comments = opening;
    comments.extend(thread.recent.items().filter(|c| c.id.as_ref().is_none_or(|id| !opened.contains(id))));
    let gap = total > comments.len();
    let thread = Thread {
        resolved: thread.is_resolved,
        resolved_by: thread.resolved_by.and_then(|u| u.login),
        resolved_at: None,
        review_id,
        notes: comments.into_iter().map(GqlComment::note).collect(),
    };
    (thread, gap)
}

fn github_snapshot(bytes: &[u8], me: &str) -> Result<Option<Snapshot>, String> {
    let answer: GqlAnswer = from_json(bytes)?;
    let Some(pull) = answer.data.and_then(|d| d.repository).and_then(|r| r.pull_request) else {
        let why = answer.errors.first().map_or("the change is not visible to this login", |e| e.message.as_str());
        return Err(format!("GitHub did not return the change: {why}"));
    };
    let mut gaps = Vec::new();
    if pull.review_threads.has_earlier() {
        gaps.push("the oldest review threads");
    }
    let commits_complete = !pull.commits.has_earlier();
    if !commits_complete {
        gaps.push("the oldest commits");
    }
    let dismissals: Vec<GqlDismissal> = pull.timeline_items.items().collect();
    let reviews_cut = pull.reviews.has_earlier();
    let latest = pull
        .reviews
        .items()
        .filter(|r| r.submitted_at.is_some() && !github_reply_only(r))
        .filter_map(|r| github_state(&r, &dismissals).map(|s| (r, s)))
        .max_by_key(|(r, _)| r.submitted_at);
    let Some((review, (state, approval_dismissed))) = latest else {
        // Rule 15: the newest 100 reviews of mine held no review, but older ones exist that were not read.
        return if reviews_cut {
            Err("GitHub returned only the newest 100 of your reviews here and none counted as a review, so an older one was not read".into())
        } else {
            Ok(None)
        };
    };
    let mut threads = Vec::new();
    let mut middle_unread = false;
    for (thread, gap) in pull.review_threads.items().map(github_thread) {
        middle_unread |= gap;
        threads.push(thread);
    }
    if middle_unread {
        gaps.push("the middle of a thread longer than 21 comments");
    }
    let requested_at = pull
        .requests
        .items()
        .filter(|e| e.requested_reviewer.as_ref().and_then(|r| r.login.as_deref()).is_some_and(|login| login.eq_ignore_ascii_case(me)))
        .filter_map(|e| e.created_at)
        .max();
    Ok(Some(Snapshot {
        me: me.to_string(),
        author: pull.author.and_then(|a| a.login).unwrap_or_else(|| "ghost".into()),
        review: MyReview { state, submitted_at: review.submitted_at, commit: review.commit.map(|c| c.oid) },
        head: pull.head_ref_oid,
        commits: pull.commits.items().map(|n| Commit { sha: n.commit.oid, at: n.commit.committed_date }).collect(),
        threads,
        notes: pull.comments.items().map(GqlComment::note).collect(),
        re_requested: pull.review_requests.items().filter_map(|r| r.requested_reviewer?.login).any(|login| login.eq_ignore_ascii_case(me)),
        re_requested_at: requested_at,
        approval_dismissed,
        review_id: Some(review.id),
        commits_complete,
        incomplete: (!gaps.is_empty()).then(|| format!("GitHub did not give all of it: {} could not be read.", gaps.join(", "))),
    }))
}

#[derive(Deserialize)]
struct GlMr {
    author: GlUser,
    /// The head commit.
    sha: String,
}

#[derive(Deserialize)]
struct GlUser {
    username: String,
}

#[derive(Deserialize)]
struct GlDiscussion {
    notes: Vec<GlNote>,
}

#[derive(Deserialize)]
struct GlNote {
    author: GlUser,
    created_at: DateTime<Utc>,
    body: String,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    resolvable: bool,
    #[serde(default)]
    resolved: bool,
    resolved_by: Option<GlUser>,
    resolved_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct GlCommit {
    id: String,
    committed_date: DateTime<Utc>,
}

/// One pushed state of the merge request. GitLab has no review objects, so this says which commit a review sat on.
#[derive(Deserialize)]
struct GlVersion {
    head_commit_sha: String,
    created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct GlApprovals {
    approved_by: Vec<GlApprover>,
}

#[derive(Deserialize)]
struct GlApprover {
    user: GlUser,
}

/// What `gitlab_snapshot` reads, already fetched.
struct GlRead {
    mr: GlMr,
    discussions: Vec<GlDiscussion>,
    /// Newest first, as GitLab lists them.
    commits: Vec<GlCommit>,
    versions: Vec<GlVersion>,
    approved_now: bool,
    commits_complete: bool,
    incomplete: Option<String>,
}

/// The most pages read of one list, 100 items each. A longer list is reported as partial.
pub(crate) const PAGES: usize = 10;

/// Every item of a paged GitLab list, and whether the pages ran out before the list did. `base` ends in `?` or `&`.
pub(crate) fn pages<T: DeserializeOwned>(host: &str, base: &str) -> Result<(Vec<T>, bool), String> {
    pages_up_to(host, base, PAGES)
}

/// [`pages`] with its own page limit; `true` means the limit was hit with a full last page.
pub(crate) fn pages_up_to<T: DeserializeOwned>(host: &str, base: &str, limit: usize) -> Result<(Vec<T>, bool), String> {
    let mut all = Vec::new();
    for page in 1..=limit {
        let got: Vec<T> = from_json(&run_on(host, "glab", &["api", &format!("{base}per_page=100&page={page}")])?)?;
        let full = got.len() >= 100;
        all.extend(got);
        if !full {
            return Ok((all, false));
        }
    }
    Ok((all, true))
}

fn joined<T>(job: thread::Result<Result<T, String>>) -> Result<T, String> {
    job.unwrap_or_else(|_| Err("the lookup stopped".into()))
}

fn gitlab_read(change: &ChangeRef, me: &str) -> Result<GlRead, String> {
    let (host, base) = (change.host_name(), format!("projects/{}/merge_requests/{}", project_path(&change.repo), change.number));
    let (mr, approvals, discussions, commits, versions) = thread::scope(|scope| {
        let discussions = scope.spawn(|| pages::<GlDiscussion>(host, &format!("{base}/discussions?")));
        let commits = scope.spawn(|| pages::<GlCommit>(host, &format!("{base}/commits?")));
        let versions = scope.spawn(|| pages::<GlVersion>(host, &format!("{base}/versions?")));
        let approvals = scope.spawn(|| run_on(host, "glab", &["api", &format!("{base}/approvals")]).and_then(|b| from_json::<GlApprovals>(&b)));
        let mr = run_on(host, "glab", &["api", &base]).and_then(|b| from_json::<GlMr>(&b));
        (mr, joined(approvals.join()), joined(discussions.join()), joined(commits.join()), joined(versions.join()))
    });
    let ((discussions, discussions_cut), (commits, commits_cut), (versions, versions_cut)) = (discussions?, commits?, versions?);
    let cut: Vec<&str> = [(discussions_cut, "discussions"), (commits_cut, "commits"), (versions_cut, "versions")]
        .into_iter()
        .filter(|(cut, _)| *cut)
        .map(|(_, what)| what)
        .collect();
    let limit = PAGES * 100;
    Ok(GlRead {
        mr: mr?,
        approved_now: approvals?.approved_by.iter().any(|a| a.user.username.eq_ignore_ascii_case(me)),
        discussions,
        commits,
        versions,
        commits_complete: !commits_cut,
        incomplete: (!cut.is_empty()).then(|| format!("GitLab has more than {limit} of its {}, so part of it was not read.", cut.join(" and "))),
    })
}

/// The head commit of the newest version pushed at or before `at`: what a review written at `at` looked at.
fn version_before(versions: &[GlVersion], at: DateTime<Utc>) -> Option<String> {
    versions.iter().filter(|v| v.created_at <= at).max_by_key(|v| v.created_at).map(|v| v.head_commit_sha.clone())
}

/// What GitLab knows of the user's review: a merge request has no review object, so it is the user's latest
/// approval note, or else the latest note the user wrote. A system note "approved this merge request" marks
/// the approval. An approval that is gone without an "unapproved" note was dismissed (for example by a push),
/// unless the user wrote a comment after it: that comment is then the latest review, and it is a comment.
fn gitlab_snapshot(me: &str, read: GlRead) -> Option<Snapshot> {
    let GlRead { mr, discussions, mut commits, versions, approved_now, commits_complete, incomplete } = read;
    commits.reverse();
    let commits: Vec<Commit> = commits.into_iter().map(|c| Commit { sha: c.id, at: c.committed_date }).collect();

    let mine = |n: &GlNote| n.author.username.eq_ignore_ascii_case(me);
    let all: Vec<&GlNote> = discussions.iter().flat_map(|d| &d.notes).collect();
    // A note I wrote inside someone else's discussion answers them; it is not a review.
    let opened_by_me = discussions.iter().filter_map(|d| d.notes.first()).filter(|n| mine(n) && !n.system);
    let last_comment = opened_by_me.map(|n| n.created_at).max();
    let approval_note = |word: &str| all.iter().filter(|n| mine(n) && n.system && n.body.starts_with(word)).map(|n| n.created_at).max();
    let (approved_at, unapproved_at) = (approval_note("approved this merge request"), approval_note("unapproved this merge request"));
    let approval_stands = approved_at.is_some_and(|a| unapproved_at.is_none_or(|u| a > u));
    let (state, submitted_at, approval_dismissed) = if approved_now {
        (ReviewState::Approved, approved_at.or(last_comment), false)
    } else if approval_stands {
        match last_comment.filter(|comment| approved_at.is_some_and(|approved| *comment > approved)) {
            Some(at) => (ReviewState::Commented, Some(at), false),
            None => (ReviewState::Approved, approved_at, true),
        }
    } else {
        let latest = [last_comment, approved_at, unapproved_at].into_iter().flatten().max()?;
        (ReviewState::Commented, Some(latest), false)
    };

    let note = |n: &GlNote| Note { author: n.author.username.clone(), at: n.created_at, body: n.body.clone() };
    let mut threads = Vec::new();
    let mut notes = Vec::new();
    for discussion in &discussions {
        let human: Vec<&GlNote> = discussion.notes.iter().filter(|n| !n.system).collect();
        // Only diff discussions resolve. A reply under a general comment of the user's counts as a mention at most.
        if human.first().is_some_and(|n| n.resolvable) {
            let resolvable: Vec<&GlNote> = human.iter().copied().filter(|n| n.resolvable).collect();
            let resolution = resolvable.iter().copied().filter(|n| n.resolved).max_by_key(|n| n.resolved_at);
            threads.push(Thread {
                resolved: resolvable.iter().all(|n| n.resolved),
                resolved_by: resolution.and_then(|n| n.resolved_by.as_ref()).map(|u| u.username.clone()),
                resolved_at: resolution.and_then(|n| n.resolved_at),
                review_id: None,
                notes: human.iter().map(|n| note(n)).collect(),
            });
        } else {
            notes.extend(human.iter().map(|n| note(n)));
        }
    }
    let asked_again = |n: &GlNote| {
        n.system
            && !mine(n)
            && submitted_at.is_some_and(|since| n.created_at > since)
            && n.body.starts_with("requested review from")
            && mentions(&n.body, me)
    };
    let re_requested_at = all.iter().copied().filter(|n| asked_again(n)).map(|n| n.created_at).max();
    Some(Snapshot {
        me: me.to_string(),
        author: mr.author.username,
        review: MyReview { state, submitted_at, commit: submitted_at.and_then(|at| version_before(&versions, at)) },
        head: mr.sha,
        commits,
        threads,
        notes,
        re_requested: re_requested_at.is_some(),
        re_requested_at,
        approval_dismissed,
        review_id: None,
        commits_complete,
        incomplete,
    })
}

/// The unified diff between two commits of `change`'s repository, for what changed since a review.
/// Callers use it only when the review's commit still belongs to the change; see `pr_craft_core::turn::rewritten`.
/// Blocks on the CLI.
pub fn compare_commits(change: &ChangeRef, from_sha: &str, to_sha: &str) -> Result<Compare, String> {
    compare_diff(change, from_sha, to_sha)
}

/// One CLI call that changes a forge. It is data first, so what a hand-off sends can be tested without sending it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub cli: &'static str,
    /// The server the call goes to.
    pub host: String,
    pub args: Vec<String>,
    pub body: Value,
}

impl Call {
    fn send(&self) -> Result<(), String> {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        run_with_input_on(&self.host, self.cli, &args, self.body.to_string().as_bytes()).map(drop)
    }
}

/// GitHub: ask `to` first, then take `from_me` off, so a failure between the two never leaves the change with no reviewer.
pub fn github_hand_off_calls(change: &ChangeRef, from_me: &str, to: &str) -> [Call; 2] {
    let path = format!("repos/{}/pulls/{}/requested_reviewers", change.repo, change.number);
    let call = |method: &str, login: &str| Call {
        cli: "gh",
        host: change.host_name().to_string(),
        args: ["api", &path, "--method", method, "--input", "-"].map(String::from).to_vec(),
        body: json!({ "reviewers": [login] }),
    };
    [call("POST", to), call("DELETE", from_me)]
}

/// GitLab replaces the whole reviewer list: the ones there now, minus the user, plus `to`.
pub fn gitlab_hand_off_call(change: &ChangeRef, current: &[u64], from_id: Option<u64>, to_id: u64) -> Call {
    let mut ids: Vec<u64> = current.iter().copied().filter(|id| Some(*id) != from_id).collect();
    if !ids.contains(&to_id) {
        ids.push(to_id);
    }
    let path = format!("projects/{}/merge_requests/{}", project_path(&change.repo), change.number);
    Call {
        cli: "glab",
        host: change.host_name().to_string(),
        args: ["api", &path, "--method", "PUT", "-H", "Content-Type: application/json", "--input", "-"].map(String::from).to_vec(),
        body: json!({ "reviewer_ids": ids }),
    }
}

#[derive(Deserialize)]
struct GlReviewers {
    #[serde(default)]
    reviewers: Vec<GlReviewer>,
}

#[derive(Deserialize)]
struct GlReviewer {
    id: u64,
    username: String,
}

#[derive(Deserialize)]
struct GlUserId {
    id: u64,
}

/// Hands the user's review of `change` to `to`: `to` is asked to review and `from_me` stops being a reviewer.
/// This writes to the forge. Blocks on the CLI.
pub fn hand_off(change: &ChangeRef, from_me: &str, to: &str) -> Result<(), String> {
    match change.forge {
        Forge::GitHub => github_hand_off_calls(change, from_me, to).iter().try_for_each(Call::send),
        Forge::GitLab => {
            let path = format!("projects/{}/merge_requests/{}", project_path(&change.repo), change.number);
            let current: GlReviewers = from_json(&run_on(change.host_name(), "glab", &["api", &path])?)?;
            let found: Vec<GlUserId> = from_json(&run_on(change.host_name(), "glab", &["api", &format!("users?username={to}")])?)?;
            let to_id = found.first().ok_or_else(|| format!("GitLab has no user named {to}"))?.id;
            let ids: Vec<u64> = current.reviewers.iter().map(|r| r.id).collect();
            let from_id = current.reviewers.iter().find(|r| r.username.eq_ignore_ascii_case(from_me)).map(|r| r.id);
            gitlab_hand_off_call(change, &ids, from_id, to_id).send()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "mo";

    // Shape of `gh api graphql` for GITHUB_QUERY; logins, ids and times invented.
    fn review(id: &str, state: &str, day: u32, oid: &str, body: &str, comments: &[Value]) -> Value {
        json!({"id": id, "state": state, "submittedAt": format!("2026-11-{day}T09:00:00Z"), "body": body, "commit": {"oid": oid},
               "comments": {"totalCount": comments.len(), "nodes": comments}})
    }

    fn own_comment() -> Value {
        json!({"replyTo": null})
    }

    fn reply_comment() -> Value {
        json!({"replyTo": {"id": "other"}})
    }

    fn comment(id: &str, author: &str, day: u32, body: &str, review: &str) -> Value {
        json!({"id": id, "author": {"login": author}, "createdAt": format!("2026-11-{day}T10:00:00Z"), "body": body, "pullRequestReview": {"id": review}})
    }

    fn thread(resolved_by: Option<&str>, opening: Value, recent: &[Value], total: u32) -> Value {
        json!({"isResolved": resolved_by.is_some(), "resolvedBy": resolved_by.map(|login| json!({"login": login})),
               "opening": {"nodes": [opening]}, "recent": {"totalCount": total, "nodes": recent}})
    }

    fn pull(reviews: &[Value], threads: &[Value], threads_cut: bool) -> Vec<u8> {
        pull_page(reviews, threads, threads_cut, false)
    }

    fn pull_page(reviews: &[Value], threads: &[Value], threads_cut: bool, reviews_cut: bool) -> Vec<u8> {
        json!({"data": {"repository": {"pullRequest": {
            "author": {"login": "nina"}, "headRefOid": "c3",
            "reviews": {"pageInfo": {"hasPreviousPage": reviews_cut}, "nodes": reviews},
            "commits": {"pageInfo": {"hasPreviousPage": false}, "nodes": [
                {"commit": {"oid": "c1", "committedDate": "2026-11-17T08:00:00Z"}},
                {"commit": {"oid": "c2", "committedDate": "2026-11-18T08:00:00Z"}},
                {"commit": {"oid": "c3", "committedDate": "2026-11-19T08:00:00Z"}}]},
            "reviewThreads": {"pageInfo": {"hasPreviousPage": threads_cut}, "nodes": threads},
            "reviewRequests": {"nodes": [{"requestedReviewer": {"login": "MO"}}, {"requestedReviewer": {}}, null]},
            "comments": {"nodes": [{"author": null, "createdAt": "2026-11-19T11:00:00Z", "body": "ping @mo"}]},
            "timelineItems": {"nodes": [{"previousReviewState": "APPROVED", "review": {"id": "R2"}}]},
            "requests": {"nodes": [
                {"createdAt": "2026-11-19T12:00:00Z", "requestedReviewer": {"login": "mo"}},
                {"createdAt": "2026-11-18T12:00:00Z", "requestedReviewer": {"login": "MO"}},
                {"createdAt": "2026-11-19T13:00:00Z", "requestedReviewer": {"login": "someone-else"}}]}}}}})
        .to_string()
        .into_bytes()
    }

    fn mine(bytes: &[u8]) -> Snapshot {
        github_snapshot(bytes, ME).expect("parses").expect("a review")
    }

    #[test]
    fn a_dismissed_github_review_is_an_approval_that_stopped_counting() {
        let reviews = [review("R1", "COMMENTED", 17, "c1", "see inline", &[own_comment()]), review("R2", "DISMISSED", 18, "c2", "", &[])];
        let s = mine(&pull(&reviews, &[], false));
        assert_eq!(s.review.state, ReviewState::Approved);
        assert!(s.approval_dismissed);
        assert_eq!(s.review.commit.as_deref(), Some("c2"), "the latest review, not the first");
        assert_eq!(s.review_id.as_deref(), Some("R2"));
        assert_eq!(s.author, "nina");
        assert_eq!((s.commits.len(), s.notes.len()), (3, 1));
        assert!(s.commits_complete);
        assert!(s.re_requested, "logins compare without case; a team request has no login and a hidden node is null");
        assert_eq!(
            s.re_requested_at.map(|at| at.to_rfc3339()).as_deref(),
            Some("2026-11-19T12:00:00+00:00"),
            "my newest request, not someone else's"
        );
        assert_eq!(s.notes[0].author, "ghost", "a deleted account has no author");
    }

    #[test]
    fn a_reply_in_someone_elses_thread_is_not_my_latest_review() {
        // R2 only answers a thread (no summary, every comment a reply); R1 is the review it follows.
        let reviews = [review("R1", "COMMENTED", 17, "c1", "", &[own_comment()]), review("R2", "COMMENTED", 19, "c3", "", &[reply_comment()])];
        let s = mine(&pull(&reviews, &[], false));
        assert_eq!(s.review_id.as_deref(), Some("R1"));
        assert_eq!(s.review.commit.as_deref(), Some("c1"), "since_last compares from R1's commit, not the reply's");
    }

    #[test]
    fn a_comment_review_with_a_summary_or_a_new_thread_still_counts() {
        let with_summary = review("R2", "COMMENTED", 19, "c3", "looks fine", &[reply_comment()]);
        let with_thread = review("R2", "COMMENTED", 19, "c3", "", &[reply_comment(), own_comment()]);
        for latest in [with_summary, with_thread] {
            let s = mine(&pull(&[review("R1", "COMMENTED", 17, "c1", "", &[own_comment()]), latest], &[], false));
            assert_eq!(s.review_id.as_deref(), Some("R2"));
        }
        let mut long = review("R2", "COMMENTED", 19, "c3", "", &[reply_comment()]);
        long["comments"]["totalCount"] = json!(31);
        let s = mine(&pull(&[review("R1", "COMMENTED", 17, "c1", "", &[own_comment()]), long], &[], false));
        assert_eq!(s.review_id.as_deref(), Some("R2"), "comments that were not read may be new threads");
    }

    #[test]
    fn threads_keep_their_review_and_resolver_and_a_partial_read_is_reported() {
        let reviews = [review("R2", "CHANGES_REQUESTED", 18, "c2", "", &[own_comment()])];
        let first = comment("t1", "mo", 18, "needs a lock", "R2");
        let reply = comment("t2", "nina", 19, "done", "R2");
        let short = thread(Some("nina"), first.clone(), &[first.clone(), reply.clone()], 2);
        let long = thread(None, first.clone(), std::slice::from_ref(&reply), 30);
        let s = mine(&pull(&reviews, &[short], false));
        let t = &s.threads[0];
        assert_eq!((t.review_id.as_deref(), t.resolved_by.as_deref(), t.resolved), (Some("R2"), Some("nina"), true));
        assert_eq!(t.notes.len(), 2, "the first comment is not counted twice when it is also among the newest");
        assert!(s.incomplete.is_none());
        let s = mine(&pull(&reviews, &[long], false));
        assert!(s.incomplete.as_deref().is_some_and(|text| text.contains("thread longer")), "{:?}", s.incomplete);
        let s = mine(&pull(&reviews, &[], true));
        assert!(s.incomplete.as_deref().is_some_and(|text| text.contains("oldest review threads")), "{:?}", s.incomplete);
    }

    #[test]
    fn a_github_change_without_my_review_is_not_reviewed() {
        assert_eq!(github_snapshot(&pull(&[], &[], false), ME), Ok(None));
        let only_a_reply = [review("R1", "COMMENTED", 17, "c1", "", &[reply_comment()])];
        assert_eq!(github_snapshot(&pull(&only_a_reply, &[], false), ME), Ok(None));
    }

    #[test]
    fn a_window_of_only_reply_reviews_with_an_earlier_page_is_an_error_not_no_review() {
        let replies: Vec<Value> = (0..20).map(|i| review(&format!("R{i}"), "COMMENTED", 17, "c1", "", &[reply_comment()])).collect();
        let error = github_snapshot(&pull_page(&replies, &[], false, true), ME).expect_err("an older review may exist");
        assert!(!error.is_empty());
        assert_eq!(github_snapshot(&pull_page(&replies, &[], false, false), ME), Ok(None), "with nothing earlier, there is none");
    }

    #[test]
    fn a_graphql_error_is_an_error_not_a_quiet_change() {
        let missing = br#"{"data":{"repository":null},"errors":[{"message":"Could not resolve to a Repository"}]}"#;
        let error = github_snapshot(missing, ME).expect_err("no change returned");
        assert!(error.contains("Could not resolve"), "{error}");
    }

    // Shape of GitLab's merge request, discussions, commits (newest first), versions (newest first) and approvals
    // endpoints; invented. Commit d3 carries an old date (made locally days before it was pushed).
    const MR: &str = r#"{"iid":88,"sha":"d3","author":{"username":"r.amini"}}"#;
    const COMMITS: &str = r#"[{"id":"d3","committed_date":"2026-11-16T07:00:00.000Z"},
        {"id":"d2","committed_date":"2026-11-17T08:00:00.000Z"},{"id":"d1","committed_date":"2026-11-16T08:00:00.000Z"}]"#;
    const VERSIONS: &str = r#"[{"head_commit_sha":"d3","created_at":"2026-11-19T08:00:00.000Z"},
        {"head_commit_sha":"d2","created_at":"2026-11-17T08:30:00.000Z"},{"head_commit_sha":"d1","created_at":"2026-11-16T09:00:00.000Z"}]"#;
    const DISCUSSIONS: &str = r#"[
        {"notes":[{"author":{"username":"mo"},"created_at":"2026-11-17T09:00:00.000Z","body":"approved this merge request","system":true}]},
        {"notes":[{"author":{"username":"mo"},"created_at":"2026-11-17T08:55:00.000Z","body":"nit","resolvable":true,"resolved":true,
                   "resolved_by":{"username":"nina"},"resolved_at":"2026-11-18T09:30:00.000Z"},
                  {"author":{"username":"r.amini"},"created_at":"2026-11-18T09:00:00.000Z","body":"ok","resolvable":true,"resolved":true,
                   "resolved_by":{"username":"nina"},"resolved_at":"2026-11-18T09:30:00.000Z"}]},
        {"notes":[{"author":{"username":"lea"},"created_at":"2026-11-18T10:00:00.000Z","body":"cc @mo","resolvable":false}]},
        {"notes":[{"author":{"username":"lea"},"created_at":"2026-11-18T11:00:00.000Z","body":"requested review from @mo","system":true}]}]"#;

    fn read(discussions: &str, approved_now: bool) -> GlRead {
        GlRead {
            mr: from_json(MR.as_bytes()).expect("mr"),
            discussions: from_json(discussions.as_bytes()).expect("discussions"),
            commits: from_json(COMMITS.as_bytes()).expect("commits"),
            versions: from_json(VERSIONS.as_bytes()).expect("versions"),
            approved_now,
            commits_complete: true,
            incomplete: None,
        }
    }

    #[test]
    fn a_gitlab_approval_that_vanished_without_an_unapprove_note_was_dismissed() {
        let s = gitlab_snapshot(ME, read(DISCUSSIONS, false)).expect("a review");
        assert_eq!(s.review.state, ReviewState::Approved);
        assert!(s.approval_dismissed);
        assert_eq!(s.commits.iter().map(|c| c.sha.as_str()).collect::<Vec<_>>(), ["d1", "d2", "d3"], "oldest first");
        assert_eq!((s.threads.len(), s.notes.len()), (1, 1), "the diff discussion is a thread, the general one a note");
        let t = &s.threads[0];
        assert_eq!((t.resolved, t.resolved_by.as_deref()), (true, Some("nina")));
        assert!(t.resolved_at.is_some());
        assert!(s.re_requested, "a system note asking me again, after my approval");
        assert_eq!(s.re_requested_at.map(|at| at.to_rfc3339()).as_deref(), Some("2026-11-18T11:00:00+00:00"));
        assert_eq!(s.head, "d3");
    }

    #[test]
    fn the_gitlab_review_commit_is_the_version_pushed_before_it_not_the_newest_commit_date() {
        // Approved 09:00 on the 17th: version d2 (08:30) was the head. Commit d3 is dated the 16th, so a commit-date rule would say d3.
        let s = gitlab_snapshot(ME, read(DISCUSSIONS, false)).expect("a review");
        assert_eq!(s.review.commit.as_deref(), Some("d2"));
        let versions: Vec<GlVersion> = from_json(VERSIONS.as_bytes()).expect("versions");
        let before =
            |hour: u32| version_before(&versions, DateTime::parse_from_rfc3339(&format!("2026-11-16T{hour:02}:00:00Z")).expect("time").into());
        assert_eq!(before(8), None, "no version existed yet");
        assert_eq!(before(10).as_deref(), Some("d1"));
    }

    #[test]
    fn a_gitlab_approval_that_still_stands_is_an_approval() {
        let s = gitlab_snapshot(ME, read(DISCUSSIONS, true)).expect("a review");
        assert_eq!((s.review.state, s.approval_dismissed), (ReviewState::Approved, false));
    }

    #[test]
    fn a_comment_after_a_gone_approval_is_a_comment_review_not_a_dismissed_approval() {
        let later = DISCUSSIONS.replacen(
            r#"{"notes":[{"author":{"username":"lea"},"created_at":"2026-11-18T10:00:00.000Z""#,
            r#"{"notes":[{"author":{"username":"mo"},"created_at":"2026-11-18T12:00:00.000Z","body":"one more thing"}]},
        {"notes":[{"author":{"username":"lea"},"created_at":"2026-11-18T10:00:00.000Z""#,
            1,
        );
        let s = gitlab_snapshot(ME, read(&later, false)).expect("a review");
        assert_eq!((s.review.state, s.approval_dismissed), (ReviewState::Commented, false));
        assert_eq!(s.review.submitted_at.map(|at| at.to_rfc3339()).as_deref(), Some("2026-11-18T12:00:00+00:00"));
        assert_eq!(s.review.commit.as_deref(), Some("d2"), "the version pushed before that comment");
    }

    #[test]
    fn a_gitlab_change_i_never_touched_is_not_reviewed() {
        let quiet = r#"[{"notes":[{"author":{"username":"lea"},"created_at":"2026-11-18T10:00:00.000Z","body":"hi"}]}]"#;
        assert!(gitlab_snapshot(ME, read(quiet, false)).is_none());
    }

    #[test]
    fn my_reply_inside_someone_elses_gitlab_discussion_is_not_a_review() {
        // lea opened the discussion; mo only answers her.
        let answer_only = r#"[{"notes":[{"author":{"username":"lea"},"created_at":"2026-11-18T10:00:00.000Z","body":"question","resolvable":true},
            {"author":{"username":"mo"},"created_at":"2026-11-18T11:00:00.000Z","body":"answer","resolvable":true}]}]"#;
        assert!(gitlab_snapshot(ME, read(answer_only, false)).is_none(), "no review of mine on this change");
        // After an approval that is gone, such an answer must not turn the dismissed approval into a comment review.
        let approved_then_answered = DISCUSSIONS.replacen(
            r#""body":"cc @mo","resolvable":false}"#,
            r#""body":"cc @mo","resolvable":false},
            {"author":{"username":"mo"},"created_at":"2026-11-18T12:00:00.000Z","body":"on it"}"#,
            1,
        );
        let s = gitlab_snapshot(ME, read(&approved_then_answered, false)).expect("a review");
        assert_eq!((s.review.state, s.approval_dismissed), (ReviewState::Approved, true));
    }

    fn change(forge: Forge, repo: &str) -> ChangeRef {
        ChangeRef { forge, repo: repo.into(), number: 482, host: None }
    }

    #[test]
    fn a_github_hand_off_asks_the_new_reviewer_before_removing_me() {
        let [add, remove] = github_hand_off_calls(&change(Forge::GitHub, "acme/api"), "mo", "j.park");
        let path = "repos/acme/api/pulls/482/requested_reviewers";
        assert_eq!(add.args, ["api", path, "--method", "POST", "--input", "-"]);
        assert_eq!(add.body, json!({ "reviewers": ["j.park"] }));
        assert_eq!(remove.args, ["api", path, "--method", "DELETE", "--input", "-"]);
        assert_eq!(remove.body, json!({ "reviewers": ["mo"] }));
    }

    #[test]
    fn a_gitlab_hand_off_swaps_me_for_the_new_reviewer_and_keeps_the_others() {
        let call = gitlab_hand_off_call(&change(Forge::GitLab, "infra/aws/modules"), &[7, 9, 12], Some(9), 31);
        assert_eq!(call.args[1], "projects/infra%2Faws%2Fmodules/merge_requests/482");
        assert_eq!(call.args[3], "PUT");
        assert_eq!(call.body, json!({ "reviewer_ids": [7, 12, 31] }));
        let again = gitlab_hand_off_call(&change(Forge::GitLab, "g/p"), &[9, 31], Some(9), 31);
        assert_eq!(again.body, json!({ "reviewer_ids": [31] }), "a reviewer already there is not added twice");
    }
}
