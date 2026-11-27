//! Review threads, read and written through `gh` and `glab`.

use pr_craft_core::review::LineRef;
use pr_craft_core::thread::{Original, ReviewThread, ThreadComment};
use pr_craft_core::{ChangeRef, DateTime, Forge, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::review::project_path;
use crate::turn::{PAGES, pages};
use crate::{from_json, run_on, run_with_input_on, run_write_on};

/// Every review thread of a change, and whether all of it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRead {
    pub threads: Vec<ReviewThread>,
    /// Every thread and every comment was read.
    pub complete: bool,
    /// What was not read, when `complete` is false.
    pub note: Option<String>,
}

/// The lines of the hunk a first comment keeps: the last ones are the code it sits on.
const HUNK_LINES: usize = 6;

/// The review threads of `change`. `head_sha` is the head commit of the diff being reviewed (GitLab's `diff_refs.head_sha`),
/// against which a GitLab discussion is judged outdated. Blocks on the CLI; call it off the UI thread.
pub fn list(change: &ChangeRef, head_sha: &str) -> Result<ThreadRead, String> {
    match change.forge {
        Forge::GitHub => github_list(change),
        Forge::GitLab => gitlab_list(change, head_sha),
    }
}

fn read_of(threads: Vec<ReviewThread>, mut gaps: Vec<String>) -> ThreadRead {
    let cut = threads.iter().filter(|t| usize::try_from(t.comments_total).is_ok_and(|total| total > t.comments.len())).count();
    if cut > 0 {
        gaps.push(format!("{cut} thread(s) have more comments than were read"));
    }
    ThreadRead { threads, complete: gaps.is_empty(), note: (!gaps.is_empty()).then(|| format!("Not all of it was read: {}.", gaps.join("; "))) }
}

// ---- GitHub: read

const GITHUB_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line originalLine startLine diffSide
          resolvedBy { login } viewerCanResolve viewerCanUnresolve viewerCanReply
          comments(first: 100) {
            totalCount
            nodes { author { login } createdAt body diffHunk originalCommit { oid } outdated }
          }
        }
      }
    }
  }
}";

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPull {
    review_threads: GqlThreads,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThreads {
    /// A node is `null` when the forge hides it from the user.
    nodes: Vec<Option<GqlThread>>,
    page_info: GqlPageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThread {
    id: String,
    is_resolved: bool,
    is_outdated: bool,
    #[serde(default)]
    path: String,
    line: Option<u32>,
    original_line: Option<u32>,
    /// `RIGHT` (the new side) or `LEFT` (the old side).
    diff_side: Option<String>,
    resolved_by: Option<GqlLogin>,
    #[serde(default)]
    viewer_can_resolve: bool,
    #[serde(default)]
    viewer_can_unresolve: bool,
    #[serde(default)]
    viewer_can_reply: bool,
    comments: GqlComments,
}

#[derive(Deserialize)]
struct GqlLogin {
    login: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlComments {
    total_count: u32,
    nodes: Vec<Option<GqlComment>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlComment {
    author: Option<GqlLogin>,
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    body: String,
    diff_hunk: Option<String>,
    original_commit: Option<GqlOid>,
}

#[derive(Deserialize)]
struct GqlOid {
    oid: String,
}

/// One page of threads and the cursor of the next, when there is one.
struct GithubPage {
    threads: Vec<ReviewThread>,
    next: Option<String>,
}

fn github_list(change: &ChangeRef) -> Result<ThreadRead, String> {
    let (owner, name) = change.repo.split_once('/').ok_or_else(|| format!("{} is not `owner/name`", change.repo))?;
    let (mut threads, mut after) = (Vec::new(), None);
    for _ in 0..PAGES {
        let page = github_page(change.host_name(), owner, name, change.number, after.as_deref())?;
        threads.extend(page.threads);
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    let gaps = after.map(|_| format!("GitHub has more than {} review threads", PAGES * 100)).into_iter().collect();
    Ok(read_of(threads, gaps))
}

fn github_page(host: &str, owner: &str, name: &str, number: u64, after: Option<&str>) -> Result<GithubPage, String> {
    let mut fields = vec![("-f", format!("query={GITHUB_QUERY}")), ("-f", format!("owner={owner}")), ("-f", format!("name={name}"))];
    fields.push(("-F", format!("number={number}")));
    fields.extend(after.map(|cursor| ("-f", format!("after={cursor}"))));
    let mut args = vec!["api", "graphql"];
    for (flag, value) in &fields {
        args.extend([*flag, value.as_str()]);
    }
    parse_github(&run_on(host, "gh", &args)?)
}

fn parse_github(bytes: &[u8]) -> Result<GithubPage, String> {
    let answer: GqlAnswer = from_json(bytes)?;
    let Some(pull) = answer.data.and_then(|d| d.repository).and_then(|r| r.pull_request) else {
        let why = answer.errors.first().map_or("the change is not visible to this login", |e| e.message.as_str());
        return Err(format!("GitHub did not return the change: {why}"));
    };
    let threads = pull.review_threads;
    let next = threads.page_info.has_next_page.then_some(threads.page_info.end_cursor).flatten();
    Ok(GithubPage { threads: threads.nodes.into_iter().flatten().map(github_thread).collect(), next })
}

/// The last lines of a diff hunk: the code a comment was written on.
fn last_lines(hunk: &str) -> Vec<String> {
    let mut lines: Vec<String> = hunk.lines().rev().take(HUNK_LINES).map(str::to_string).collect();
    lines.reverse();
    lines
}

fn github_thread(thread: GqlThread) -> ReviewThread {
    let first = thread.comments.nodes.iter().flatten().next();
    let original = Original {
        commit: first.and_then(|c| c.original_commit.as_ref()).map(|o| o.oid.clone()),
        line: thread.original_line,
        code: first.and_then(|c| c.diff_hunk.as_deref()).map(last_lines).unwrap_or_default(),
    };
    let line = thread.line.map(|n| match thread.diff_side.as_deref() {
        Some("LEFT") => LineRef { old: Some(n), new: None },
        _ => LineRef { old: None, new: Some(n) },
    });
    ReviewThread {
        id: thread.id,
        path: thread.path,
        line,
        original,
        resolved: thread.is_resolved,
        resolved_by: thread.resolved_by.and_then(|u| u.login),
        outdated: thread.is_outdated,
        can_resolve: if thread.is_resolved { thread.viewer_can_unresolve } else { thread.viewer_can_resolve },
        can_reply: thread.viewer_can_reply,
        comments: thread
            .comments
            .nodes
            .into_iter()
            .flatten()
            .map(|c| ThreadComment { author: c.author.and_then(|a| a.login).unwrap_or_else(|| "ghost".into()), at: c.created_at, body: c.body })
            .collect(),
        comments_total: thread.comments.total_count,
    }
}

// ---- GitLab: read

#[derive(Deserialize)]
struct GlDiscussion {
    id: String,
    notes: Vec<GlNote>,
}

#[derive(Deserialize)]
struct GlUser {
    username: String,
}

#[derive(Deserialize)]
struct GlNote {
    author: Option<GlUser>,
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    resolvable: bool,
    #[serde(default)]
    resolved: bool,
    resolved_by: Option<GlUser>,
    resolved_at: Option<DateTime<Utc>>,
    /// Set on a note written on a line of the diff.
    position: Option<GlPosition>,
}

#[derive(Deserialize)]
struct GlPosition {
    old_path: Option<String>,
    new_path: Option<String>,
    old_line: Option<u32>,
    new_line: Option<u32>,
    head_sha: Option<String>,
}

fn gitlab_list(change: &ChangeRef, head_sha: &str) -> Result<ThreadRead, String> {
    let base = format!("projects/{}/merge_requests/{}/discussions?", project_path(&change.repo), change.number);
    let (discussions, cut) = pages::<GlDiscussion>(change.host_name(), &base)?;
    let gaps = cut.then(|| format!("GitLab has more than {} discussions", PAGES * 100)).into_iter().collect();
    Ok(read_of(parse_gitlab(discussions, head_sha), gaps))
}

fn parse_gitlab(discussions: Vec<GlDiscussion>, head_sha: &str) -> Vec<ReviewThread> {
    discussions.into_iter().filter_map(|d| gitlab_thread(d, head_sha)).collect()
}

/// A discussion on a line of the diff. Discussions of the whole change, and system notes, are not threads.
fn gitlab_thread(discussion: GlDiscussion, head_sha: &str) -> Option<ReviewThread> {
    let notes: Vec<GlNote> = discussion.notes.into_iter().filter(|n| !n.system).collect();
    let first = notes.first()?;
    let position = first.position.as_ref()?;
    let outdated = position.head_sha.as_deref().is_some_and(|sha| sha != head_sha);
    let placed = LineRef { old: position.old_line, new: position.new_line };
    let resolvable: Vec<&GlNote> = notes.iter().filter(|n| n.resolvable).collect();
    let resolution = resolvable.iter().copied().filter(|n| n.resolved).max_by_key(|n| n.resolved_at);
    let thread = ReviewThread {
        id: discussion.id,
        path: position.new_path.clone().or_else(|| position.old_path.clone()).unwrap_or_default(),
        line: (!outdated && (placed.old.is_some() || placed.new.is_some())).then_some(placed),
        original: Original { commit: position.head_sha.clone(), line: position.new_line.or(position.old_line), code: Vec::new() },
        resolved: !resolvable.is_empty() && resolvable.iter().all(|n| n.resolved),
        resolved_by: resolution.and_then(|n| n.resolved_by.as_ref()).map(|u| u.username.clone()),
        outdated,
        can_resolve: first.resolvable,
        can_reply: true,
        comments: Vec::new(),
        comments_total: 0,
    };
    let comments: Vec<ThreadComment> = notes
        .into_iter()
        .map(|n| ThreadComment { author: n.author.map_or_else(|| "ghost".into(), |a| a.username), at: n.created_at, body: n.body })
        .collect();
    Some(ReviewThread { comments_total: u32::try_from(comments.len()).unwrap_or(u32::MAX), comments, ..thread })
}

// ---- Writes

/// A write the CLI will do. Built purely so tests can check it; [`execute`] runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub cli: &'static str,
    /// The server the call goes to.
    pub host: String,
    pub args: Vec<String>,
    /// Piped to the CLI's stdin.
    pub input: Option<String>,
}

const GITHUB_REPLY: &str = "mutation($thread: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, body: $body}) { comment { id } }
}";
const GITHUB_RESOLVE: &str = "mutation($thread: ID!) { resolveReviewThread(input: {threadId: $thread}) { thread { id isResolved } } }";
const GITHUB_UNRESOLVE: &str = "mutation($thread: ID!) { unresolveReviewThread(input: {threadId: $thread}) { thread { id isResolved } } }";

/// A thread id goes into a URL on GitLab and into a variable on GitHub; both forges' ids are plain tokens.
fn checked_id(thread_id: &str) -> Result<&str, String> {
    if !thread_id.is_empty() && thread_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
        Ok(thread_id)
    } else {
        Err(format!("{thread_id:?} is not a thread id"))
    }
}

fn github_request(query: &str, variables: &[(&str, &str)], change: &ChangeRef) -> Request {
    let mut args: Vec<String> = ["api", "graphql", "-f"].map(String::from).to_vec();
    args.push(format!("query={query}"));
    for (name, value) in variables {
        args.extend(["-f".to_string(), format!("{name}={value}")]);
    }
    Request { cli: "gh", host: change.host_name().to_string(), args, input: None }
}

fn gitlab_request(change: &ChangeRef, method: &str, tail: &str, body: &Value) -> Request {
    let path = format!("projects/{}/merge_requests/{}/discussions/{tail}", project_path(&change.repo), change.number);
    let args = ["api", &path, "--method", method, "-H", "Content-Type: application/json", "--input", "-"].map(String::from).to_vec();
    Request { cli: "glab", host: change.host_name().to_string(), args, input: Some(body.to_string()) }
}

/// Answers a thread. Refuses an empty body. The text travels as a variable (GitHub) or a JSON value (GitLab), never in the query.
pub fn reply_request(change: &ChangeRef, thread_id: &str, body: &str) -> Result<Request, String> {
    let id = checked_id(thread_id)?;
    if body.trim().is_empty() {
        return Err("A reply needs some text.".into());
    }
    Ok(match change.forge {
        Forge::GitHub => github_request(GITHUB_REPLY, &[("thread", id), ("body", body)], change),
        Forge::GitLab => gitlab_request(change, "POST", &format!("{id}/notes"), &json!({ "body": body })),
    })
}

/// Resolves a thread, or reopens it with `resolved` false.
pub fn resolve_request(change: &ChangeRef, thread_id: &str, resolved: bool) -> Result<Request, String> {
    let id = checked_id(thread_id)?;
    Ok(match change.forge {
        Forge::GitHub => github_request(if resolved { GITHUB_RESOLVE } else { GITHUB_UNRESOLVE }, &[("thread", id)], change),
        Forge::GitLab => gitlab_request(change, "PUT", id, &json!({ "resolved": resolved })),
    })
}

/// Sends `request` to the forge. GitHub's GraphQL answers can carry `errors` next to a clean exit; they fail here too.
pub fn execute(request: &Request) -> Result<(), String> {
    let args: Vec<&str> = request.args.iter().map(String::as_str).collect();
    let out = match &request.input {
        Some(input) => run_with_input_on(&request.host, request.cli, &args, input.as_bytes())?,
        None => run_write_on(&request.host, request.cli, &args)?,
    };
    match serde_json::from_slice::<Value>(&out).ok().as_ref().and_then(|v| v.get("errors")).and_then(Value::as_array).and_then(|e| e.first()) {
        Some(error) => Err(format!("{} refused it: {}", request.cli, error.get("message").and_then(Value::as_str).unwrap_or("unknown error"))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(host: Option<&str>, repo: &str) -> ChangeRef {
        ChangeRef { forge: Forge::GitHub, repo: repo.into(), number: 7, host: host.map(String::from) }
    }

    fn gitlab(host: Option<&str>) -> ChangeRef {
        ChangeRef { forge: Forge::GitLab, repo: "infra/aws/modules".into(), number: 88, host: host.map(String::from) }
    }

    // Shaped like `gh api graphql` over `reviewThreads`: names invented; one resolved thread, one outdated with a null line, one
    // null node (hidden from the user), and a comment whose author was deleted.
    const GITHUB_ANSWER: &str = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
        "pageInfo":{"hasNextPage":true,"endCursor":"Y3Vyc29yOjI="},
        "nodes":[
        {"id":"PRRT_a1","isResolved":true,"isOutdated":false,"path":"src/queue.rs","line":12,"originalLine":12,"startLine":null,"diffSide":"RIGHT",
         "resolvedBy":{"login":"nina.k"},"viewerCanResolve":false,"viewerCanUnresolve":true,"viewerCanReply":true,
         "comments":{"totalCount":2,"nodes":[
           {"author":{"login":"r.amini"},"createdAt":"2026-11-17T03:34:10Z","body":"Why retry here?",
            "diffHunk":"@@ -1,3 +1,9 @@\n a\n b\n c\n d\n e\n f\n+g","originalCommit":{"oid":"c0ffee1"},"outdated":false},
           {"author":null,"createdAt":"2026-11-17T04:00:00Z","body":"Fixed.","diffHunk":"@@","originalCommit":{"oid":"c0ffee2"},"outdated":false}]}},
        null,
        {"id":"PRRT_b2","isResolved":false,"isOutdated":true,"path":"src/old.rs","line":null,"originalLine":40,"diffSide":"LEFT",
         "resolvedBy":null,"viewerCanResolve":false,"viewerCanReply":true,
         "comments":{"totalCount":150,"nodes":[
           {"author":{"login":"r.amini"},"createdAt":"2026-11-16T03:34:10Z","body":"Dead code.","diffHunk":null,"originalCommit":{"oid":"deadbee"},"outdated":true}]}}
        ]}}}}}"#;

    #[test]
    fn github_threads_keep_their_resolution_placement_and_original() {
        let page = parse_github(GITHUB_ANSWER.as_bytes()).expect("the answer parses");
        assert_eq!(page.next.as_deref(), Some("Y3Vyc29yOjI="), "a next page is named by its cursor");
        let [done, gone] = page.threads.as_slice() else { panic!("the null node is skipped, got {} threads", page.threads.len()) };
        assert!(done.resolved && done.resolved_by.as_deref() == Some("nina.k") && done.can_resolve);
        assert_eq!(done.line, Some(LineRef { old: None, new: Some(12) }));
        assert_eq!(done.comments[1].author, "ghost", "a deleted author reads as ghost");
        assert_eq!((done.original.commit.as_deref(), done.original.line), (Some("c0ffee1"), Some(12)));
        assert_eq!(done.original.code.len(), HUNK_LINES);
        assert_eq!(done.original.code.last().map(String::as_str), Some("+g"), "the code is the end of the hunk");
        assert!(gone.outdated && gone.line.is_none(), "an outdated thread has no line in the current diff");
        assert!(!gone.can_resolve && gone.original.code.is_empty());
        assert!(read_of(page.threads, Vec::new()).note.is_some_and(|n| n.contains("more comments")), "150 comments, 1 read");
    }

    #[test]
    fn a_github_answer_without_the_change_is_an_error() {
        assert!(
            parse_github(br#"{"data":{"repository":null},"errors":[{"message":"Could not resolve"}]}"#)
                .is_err_and(|e| e.contains("Could not resolve"))
        );
    }

    // Shaped like GitLab's `merge_requests/:iid/discussions`: a placed thread, one written on an older head, a resolved one,
    // a general comment, and a system note.
    const GITLAB_ANSWER: &str = r#"[
        {"id":"aa11","notes":[{"author":{"username":"r.amini"},"created_at":"2026-11-17T03:34:10Z","body":"Pin it","system":false,
          "resolvable":true,"resolved":false,"position":{"old_path":"a.tf","new_path":"a.tf","old_line":null,"new_line":9,"head_sha":"h2"}},
          {"author":{"username":"nina.k"},"created_at":"2026-11-17T05:00:00Z","body":"Done","system":false,"resolvable":true,"resolved":false},
          {"author":{"username":"x"},"created_at":"2026-11-17T05:01:00Z","body":"assigned","system":true}]},
        {"id":"bb22","notes":[{"author":{"username":"r.amini"},"created_at":"2026-11-10T03:34:10Z","body":"Old","system":false,
          "resolvable":true,"resolved":false,"position":{"old_path":"b.tf","new_path":"b.tf","old_line":3,"new_line":3,"head_sha":"h1"}}]},
        {"id":"cc33","notes":[{"author":{"username":"r.amini"},"created_at":"2026-11-10T03:34:10Z","body":"Ok","system":false,
          "resolvable":true,"resolved":true,"resolved_by":{"username":"nina.k"},"resolved_at":"2026-11-11T00:00:00Z",
          "position":{"old_path":"c.tf","new_path":"c.tf","old_line":null,"new_line":1,"head_sha":"h2"}}]},
        {"id":"dd44","notes":[{"author":{"username":"r.amini"},"created_at":"2026-11-10T03:34:10Z","body":"General","system":false,"resolvable":false}]},
        {"id":"ee55","notes":[{"author":{"username":"x"},"created_at":"2026-11-10T03:34:10Z","body":"mentioned in commit","system":true}]}
    ]"#;

    #[test]
    fn gitlab_discussions_become_threads_and_only_diff_ones() {
        let discussions: Vec<GlDiscussion> = from_json(GITLAB_ANSWER.as_bytes()).expect("fixture parses");
        let threads = parse_gitlab(discussions, "h2");
        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["aa11", "bb22", "cc33"], "a general comment and a system note are not threads");
        let placed = &threads[0];
        assert_eq!(placed.line, Some(LineRef { old: None, new: Some(9) }));
        assert_eq!((placed.comments.len(), placed.comments_total), (2, 2), "the system note is not a comment");
        assert!(!placed.resolved && !placed.outdated && placed.can_resolve);
        assert!(threads[1].outdated && threads[1].line.is_none(), "a position on an older head is outdated");
        assert_eq!((threads[1].original.commit.as_deref(), threads[1].original.line), (Some("h1"), Some(3)));
        assert!(threads[2].resolved && threads[2].resolved_by.as_deref() == Some("nina.k"));
    }

    #[test]
    fn a_reply_carries_its_text_as_data_on_both_forges() {
        let body = "ok } \"quoted\" \\ mutation { x }";
        let gh = reply_request(&github(None, "acme/api"), "PRRT_a1", body).expect("builds");
        let query = gh.args.iter().find_map(|a| a.strip_prefix("query=")).expect("a query");
        assert!(!query.contains(body) && !query.contains("PRRT_a1"), "nothing the user typed is in the query text");
        assert!(gh.args.contains(&format!("body={body}")) && gh.args.contains(&"thread=PRRT_a1".to_string()));
        let gl = reply_request(&gitlab(Some("gitlab.acme.dev")), "aa11", body).expect("builds");
        assert_eq!((gl.cli, gl.host.as_str()), ("glab", "gitlab.acme.dev"), "aimed at the self-hosted server");
        assert!(gl.args.iter().any(|a| a == "projects/infra%2Faws%2Fmodules/merge_requests/88/discussions/aa11/notes"));
        let sent: Value = serde_json::from_str(gl.input.as_deref().expect("a JSON body")).expect("JSON");
        assert_eq!(sent["body"], body);
    }

    #[test]
    fn resolving_names_the_direction_and_refuses_what_is_not_an_id() {
        let up = resolve_request(&github(None, "acme/api"), "PRRT_a1", true).expect("builds");
        let down = resolve_request(&github(None, "acme/api"), "PRRT_a1", false).expect("builds");
        assert!(up.args.iter().any(|a| a.contains("resolveReviewThread") && !a.contains("unresolve")));
        assert!(down.args.iter().any(|a| a.contains("unresolveReviewThread")));
        let gl = resolve_request(&gitlab(None), "aa11", false).expect("builds");
        assert!(gl.args.iter().any(|a| a == "PUT") && gl.args.iter().any(|a| a.ends_with("/discussions/aa11")));
        assert_eq!(serde_json::from_str::<Value>(gl.input.as_deref().expect("body")).expect("JSON")["resolved"], false);
        assert!(resolve_request(&gitlab(None), "../x", true).is_err(), "an id never reaches a URL unchecked");
    }

    #[test]
    fn an_empty_reply_is_refused() {
        for forge_change in [github(None, "acme/api"), gitlab(None)] {
            assert!(reply_request(&forge_change, "aa11", "  \n").is_err());
        }
    }
}
