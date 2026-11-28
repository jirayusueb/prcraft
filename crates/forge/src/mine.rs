//! Your changes: reads the open changes the user wrote, one change in full, its checks and a job's log, through `gh` and `glab`.
//! Everything here reads; the writes are in [`crate::act`].

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::thread;

use pr_craft_core::mine::{
    ChangesRequest, Check, CheckState, Checks, Decision, JobLog, LinkedIssue, MergeMethod, MergeQueue, MergeState, Mine, MineDetail, parse_log_cut,
};
use pr_craft_core::{Change, ChangeRef, DateTime, Forge, Utc};
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::review::project_path;
use crate::turn::{pages, pages_up_to};
use crate::{PAGE, from_json, run_on};

/// A GraphQL connection. A node is `null` when the forge hides it from the user.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Conn<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
    total_count: Option<u32>,
}

impl<T> Conn<T> {
    fn items(self) -> impl Iterator<Item = T> {
        self.nodes.into_iter().flatten()
    }
}

#[derive(Deserialize)]
struct Answer<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

impl<T> Answer<T> {
    fn data(self) -> Result<T, crate::ForgeError> {
        let why = self.errors.first().map_or("it answered with nothing", |e| e.message.as_str()).to_string();
        self.data.ok_or_else(|| format!("GitHub did not return the change: {why}").into())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchData {
    search: Search,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Search {
    page_info: PageInfo,
    nodes: Vec<Option<Pr>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoData {
    repository: Option<Repo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repo {
    #[serde(default)]
    squash_merge_allowed: bool,
    #[serde(default)]
    merge_commit_allowed: bool,
    #[serde(default)]
    rebase_merge_allowed: bool,
    pull_request: Option<Pr>,
}

#[derive(Deserialize)]
struct Login {
    login: Option<String>,
    slug: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    title: String,
    url: String,
    updated_at: DateTime<Utc>,
    is_draft: bool,
    additions: u64,
    deletions: u64,
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
    mergeable: String,
    merge_state_status: String,
    review_decision: Option<String>,
    repository: NameWithOwner,
    author: Option<Login>,
    commits: Conn<IgnoredAny>,
    review_requests: Conn<Requested>,
    latest_opinionated_reviews: Conn<Opinion>,
    review_threads: Conn<GqlThread>,
    auto_merge_request: Option<IgnoredAny>,
    head: Conn<HeadNode>,
    base_ref: Option<BaseRef>,
    labels: Conn<Named>,
    closing_issues_references: Conn<GqlIssue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NameWithOwner {
    name_with_owner: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Requested {
    requested_reviewer: Option<Login>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Opinion {
    state: String,
    submitted_at: Option<DateTime<Utc>>,
    author: Option<Login>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThread {
    is_resolved: bool,
    comments: Conn<LastComment>,
}

#[derive(Deserialize)]
struct LastComment {
    author: Option<Login>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadNode {
    commit: HeadCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadCommit {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    state: String,
    contexts: Conn<CheckNode>,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum CheckNode {
    CheckRun {
        name: String,
        status: String,
        conclusion: Option<String>,
    },
    StatusContext {
        context: String,
        state: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaseRef {
    branch_protection_rule: Option<Rule>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    required_approving_review_count: Option<u32>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct GqlIssue {
    number: u64,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueData {
    repository: Option<QueueRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueRepo {
    merge_queue: Option<GqlQueue>,
}

#[derive(Deserialize)]
struct GqlQueue {
    entries: Conn<QueueEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueEntry {
    estimated_time_to_merge: Option<u64>,
    pull_request: Option<GqlNumber>,
}

#[derive(Deserialize)]
struct GqlNumber {
    number: u64,
}

/// The fields of one pull request that both the list and the full read take.
const PR_FIELDS: &str = "fragment PrFields on PullRequest {
  number title url updatedAt isDraft additions deletions headRefName headRefOid baseRefName
  mergeable mergeStateStatus reviewDecision
  repository { nameWithOwner }
  author { login }
  commits { totalCount }
  reviewRequests(first: 30) { nodes { requestedReviewer { ... on User { login } ... on Team { slug } } } }
  latestOpinionatedReviews(first: 30) { nodes { state submittedAt author { login } } }
  reviewThreads(first: 50) { totalCount nodes { isResolved comments(last: 1) { nodes { author { login } } } } }
  autoMergeRequest { enabledAt }
  baseRef { branchProtectionRule { requiredApprovingReviewCount } }
  labels(first: 30) { nodes { name } }
  closingIssuesReferences(first: 10) { nodes { number title } }
  head: commits(last: 1) { nodes { commit { statusCheckRollup { state contexts(first: 50) { totalCount nodes {
    __typename ... on CheckRun { name status conclusion } ... on StatusContext { context state } } } } } } }
}";

fn login_of(user: &Option<Login>) -> Option<String> {
    user.as_ref().and_then(|u| u.login.clone().or_else(|| u.slug.clone()))
}

/// The state of one check run or status of a GitHub commit.
fn context_state(context: &CheckNode) -> Option<(String, CheckState)> {
    let ended = |conclusion: &Option<String>| match conclusion.as_deref() {
        Some("SUCCESS") => CheckState::Passing,
        Some("NEUTRAL" | "SKIPPED") => CheckState::Skipped,
        _ => CheckState::Failing,
    };
    match context {
        CheckNode::CheckRun { name, status, conclusion } => {
            Some((name.clone(), if status == "COMPLETED" { ended(conclusion) } else { CheckState::Running }))
        }
        CheckNode::StatusContext { context, state } => Some((
            context.clone(),
            match state.as_str() {
                "SUCCESS" => CheckState::Passing,
                "PENDING" | "EXPECTED" => CheckState::Running,
                _ => CheckState::Failing,
            },
        )),
        CheckNode::Other => None,
    }
}

/// The checks of the head commit. When the commit holds more than were read, the forge's own verdict (`state`) decides the overall
/// state and the counts say how many were read.
fn github_checks(rollup: Option<Rollup>) -> Checks {
    let Some(Rollup { state, contexts }) = rollup else { return Checks::default() };
    let total = contexts.total_count.unwrap_or(0);
    let read: Vec<(String, CheckState)> = contexts.items().filter_map(|c| context_state(&c)).collect();
    let list: Vec<Check> = read
        .iter()
        .map(|(name, state)| Check { id: name.clone(), name: name.clone(), group: None, state: *state, seconds: None, url: None, has_log: false })
        .collect();
    let mut checks = Checks::of(&list);
    checks.total = checks.total.max(total);
    checks.state = match state.as_str() {
        "FAILURE" | "ERROR" => Some(CheckState::Failing),
        "PENDING" | "EXPECTED" => Some(CheckState::Running),
        "SUCCESS" => Some(CheckState::Passing),
        _ => checks.state,
    };
    checks
}

fn github_merge(pr: &Pr) -> (MergeState, Option<String>) {
    match (pr.mergeable.as_str(), pr.merge_state_status.as_str()) {
        ("CONFLICTING", _) | (_, "DIRTY") => (MergeState::Conflicts, None),
        (_, "BEHIND") => (MergeState::Behind, None),
        (_, "CLEAN" | "HAS_HOOKS" | "UNSTABLE") => (MergeState::Ready, None),
        (_, "BLOCKED") => (MergeState::Blocked, Some("A required review or check is missing".into())),
        (_, "DRAFT") => (MergeState::Blocked, Some("It is a draft".into())),
        _ => (MergeState::Unknown, None),
    }
}

fn github_mine(pr: Pr, host: &str) -> Mine {
    let (merge, merge_note) = github_merge(&pr);
    let me = login_of(&pr.author).unwrap_or_default();
    let requested: Vec<String> = pr.review_requests.items().filter_map(|r| login_of(&r.requested_reviewer)).collect();
    let opinions: Vec<Opinion> = pr.latest_opinionated_reviews.items().collect();
    let mut reviewers = requested.clone();
    for who in opinions.iter().filter_map(|o| login_of(&o.author)) {
        if !reviewers.contains(&who) {
            reviewers.push(who);
        }
    }
    let changes_requested = opinions
        .iter()
        .filter(|o| o.state == "CHANGES_REQUESTED")
        .filter_map(|o| Some(ChangesRequest { by: login_of(&o.author)?, at: o.submitted_at }))
        .collect();
    let thread_total = pr.review_threads.total_count;
    let threads: Vec<GqlThread> = pr.review_threads.items().collect();
    let open: Vec<&GqlThread> = threads.iter().filter(|t| !t.is_resolved).collect();
    let waiting = open
        .iter()
        .filter(|t| {
            t.comments.nodes.iter().flatten().next_back().is_some_and(|c| login_of(&c.author).is_some_and(|who| !who.eq_ignore_ascii_case(&me)))
        })
        .count();
    let decision = match pr.review_decision.as_deref() {
        Some("APPROVED") => Decision::Approved,
        Some("CHANGES_REQUESTED") => Decision::ChangesRequested,
        Some("REVIEW_REQUIRED") => Decision::ReviewRequired,
        _ => Decision::None,
    };
    let rollup = pr.head.items().next().and_then(|n| n.commit.status_check_rollup);
    let mut unread = Vec::new();
    if thread_total.is_some_and(|n| n as usize > threads.len()) {
        unread.push("the threads past the first 50".to_string());
    }
    Mine {
        change: Change {
            forge: Forge::GitHub,
            repo: pr.repository.name_with_owner,
            number: pr.number,
            title: pr.title,
            author: me,
            url: pr.url,
            host: Forge::GitHub.host_field(host),
            updated_at: pr.updated_at,
            stack: None,
            draft: pr.is_draft,
        },
        additions: Some(pr.additions),
        deletions: Some(pr.deletions),
        head_branch: pr.head_ref_name,
        head_sha: pr.head_ref_oid,
        base_branch: pr.base_ref_name,
        commits: pr.commits.total_count,
        reviewers,
        requested,
        decision,
        approvals: opinions.iter().filter(|o| o.state == "APPROVED").count() as u32,
        approvals_required: pr.base_ref.and_then(|b| b.branch_protection_rule).and_then(|r| r.required_approving_review_count).filter(|n| *n > 0),
        changes_requested,
        threads_open: open.len() as u32,
        threads_waiting: waiting as u32,
        checks: github_checks(rollup),
        merge,
        merge_note,
        auto_merge: pr.auto_merge_request.is_some(),
        labels: pr.labels.items().map(|l| l.name).collect(),
        unread,
    }
}

/// Parses `gh api graphql` output of the list query: the changes and whether the host holds more than one page.
fn parse_github_list(bytes: &[u8], host: &str) -> Result<(Vec<Mine>, bool), crate::ForgeError> {
    let found = from_json::<Answer<SearchData>>(bytes)?.data()?.search;
    Ok((found.nodes.into_iter().flatten().map(|pr| github_mine(pr, host)).collect(), found.page_info.has_next_page))
}

/// One `gh api graphql` call: `fields` are `(flag, key=value)` pairs, `-F` for a typed one.
fn graphql(host: &str, query: &str, fields: &[(&str, String)]) -> Result<Vec<u8>, crate::ForgeError> {
    let query = format!("query={query}");
    let mut args = vec!["api", "graphql", "-f", query.as_str()];
    for (flag, field) in fields {
        args.extend([*flag, field.as_str()]);
    }
    run_on(host, "gh", &args)
}

/// The user's open changes on `host`, newest update first, and whether the host holds more than one page ([`PAGE`]). GitHub reads
/// everything in one search; GitLab reads the list and then each change on its own. Blocks on the CLI.
pub fn mine_on(forge: Forge, host: &str) -> Result<(Vec<Mine>, bool, Option<crate::RateLimit>), crate::ForgeError> {
    match forge {
        Forge::GitHub => {
            let query = format!(
                "query($q: String!) {{ search(query: $q, type: ISSUE, first: {PAGE}) {{ pageInfo {{ hasNextPage }} nodes {{ ...PrFields }} }} rateLimit {{ cost remaining resetAt }} }} {PR_FIELDS}"
            );
            let bytes = graphql(host, &query, &[("-f", "q=is:pr is:open author:@me sort:updated-desc".to_string())])?;
            let (found, more) = parse_github_list(&bytes, host)?;
            Ok((found, more, crate::rate_limit(&bytes)))
        }
        Forge::GitLab => gitlab_list(host).map(|(found, more)| (found, more, None)),
    }
}

#[derive(Deserialize)]
struct GlUser {
    username: String,
}

#[derive(Deserialize)]
struct GlRefs {
    /// `group/subgroup/project!88`.
    full: String,
}

#[derive(Deserialize)]
struct GlPipeline {
    status: String,
}

#[derive(Deserialize)]
struct GlMr {
    iid: u64,
    title: String,
    web_url: String,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    work_in_progress: bool,
    author: GlUser,
    references: GlRefs,
    source_branch: String,
    target_branch: String,
    #[serde(default)]
    sha: String,
    detailed_merge_status: Option<String>,
    #[serde(default)]
    has_conflicts: bool,
    #[serde(default)]
    reviewers: Vec<GlUser>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    user_notes_count: u32,
    #[serde(default)]
    merge_when_pipeline_succeeds: bool,
    /// Only the single-change read carries it.
    head_pipeline: Option<GlPipeline>,
}

#[derive(Deserialize)]
struct GlApprovals {
    #[serde(default)]
    approvals_required: u32,
    #[serde(default)]
    approvals_left: u32,
    #[serde(default)]
    approved_by: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
struct GlDiscussion {
    notes: Vec<GlNote>,
}

#[derive(Deserialize)]
struct GlNote {
    author: GlUser,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    resolvable: bool,
    #[serde(default)]
    resolved: bool,
}

/// The pipeline of a merge request as a state of its checks.
fn gitlab_pipeline_state(status: &str) -> Option<CheckState> {
    match status {
        "success" => Some(CheckState::Passing),
        "failed" | "canceled" => Some(CheckState::Failing),
        "running" | "pending" | "created" | "preparing" | "waiting_for_resource" | "scheduled" => Some(CheckState::Running),
        _ => None,
    }
}

fn gitlab_merge(mr: &GlMr) -> (MergeState, Option<String>) {
    let status = mr.detailed_merge_status.as_deref().unwrap_or_default();
    if mr.has_conflicts || status == "conflict" {
        return (MergeState::Conflicts, None);
    }
    match status {
        "mergeable" => (MergeState::Ready, None),
        "need_rebase" => (MergeState::Behind, None),
        "" | "checking" | "unchecked" | "preparing" => (MergeState::Unknown, None),
        other => (MergeState::Blocked, Some(other.replace('_', " "))),
    }
}

fn gitlab_mine(mr: GlMr, host: &str) -> Mine {
    let (merge, merge_note) = gitlab_merge(&mr);
    let status = mr.detailed_merge_status.as_deref().unwrap_or_default();
    let repo = mr.references.full.rsplit_once('!').map_or(mr.references.full.as_str(), |(path, _)| path).to_string();
    let reviewers: Vec<String> = mr.reviewers.iter().map(|u| u.username.clone()).collect();
    let checks = Checks { state: mr.head_pipeline.as_ref().and_then(|p| gitlab_pipeline_state(&p.status)), ..Checks::default() };
    Mine {
        change: Change {
            forge: Forge::GitLab,
            repo,
            number: mr.iid,
            title: mr.title,
            author: mr.author.username,
            url: mr.web_url,
            host: Forge::GitLab.host_field(host),
            updated_at: mr.updated_at,
            stack: None,
            draft: mr.draft || mr.work_in_progress,
        },
        additions: None,
        deletions: None,
        head_branch: mr.source_branch,
        head_sha: mr.sha,
        base_branch: mr.target_branch,
        commits: None,
        requested: reviewers.clone(),
        reviewers,
        decision: if status == "requested_changes" { Decision::ChangesRequested } else { Decision::None },
        approvals: 0,
        approvals_required: None,
        changes_requested: Vec::new(),
        threads_open: 0,
        threads_waiting: 0,
        checks,
        merge,
        merge_note,
        auto_merge: mr.merge_when_pipeline_succeeds,
        labels: mr.labels,
        unread: Vec::new(),
    }
}

fn mr_base(change: &ChangeRef) -> String {
    format!("projects/{}/merge_requests/{}", project_path(&change.repo), change.number)
}

/// Fills in what the list does not carry: the pipeline, the approvals, the open threads and the size. A read that fails is named in
/// `unread`, so its number is never taken for zero.
fn gitlab_fill(mine: &mut Mine, notes: u32) {
    let change = mine.change.change_ref();
    let (host, base) = (change.host_name().to_string(), mr_base(&change));
    let (single, approvals, discussions, size) = thread::scope(|scope| {
        let (host, base) = (host.as_str(), base.as_str());
        let approvals = scope.spawn(move || run_on(host, "glab", &["api", &format!("{base}/approvals")]).and_then(|b| from_json::<GlApprovals>(&b)));
        let discussions =
            scope.spawn(move || if notes == 0 { Ok((Vec::new(), false)) } else { pages::<GlDiscussion>(host, &format!("{base}/discussions?")) });
        let size = scope.spawn(|| diff_size(&change));
        let single = run_on(host, "glab", &["api", base]).and_then(|b| from_json::<GlMr>(&b));
        (
            single,
            approvals.join().unwrap_or_else(|_| Err("stopped".into())),
            discussions.join().unwrap_or_else(|_| Err("stopped".into())),
            size.join(),
        )
    });
    match single {
        Ok(mr) => {
            let fresh = gitlab_mine(mr, &host);
            mine.checks = fresh.checks;
            mine.merge = fresh.merge;
            mine.merge_note = fresh.merge_note;
            mine.decision = fresh.decision;
            mine.head_sha = fresh.head_sha;
            mine.auto_merge = fresh.auto_merge;
            mine.change.draft = fresh.change.draft;
        }
        Err(e) => mine.unread.push(format!("the pipeline ({e})")),
    }
    match approvals {
        Ok(a) => {
            mine.approvals = a.approved_by.len() as u32;
            mine.approvals_required = (a.approvals_required > 0).then_some(a.approvals_required);
            if a.approvals_left == 0 && !a.approved_by.is_empty() && mine.decision != Decision::ChangesRequested {
                mine.decision = Decision::Approved;
            } else if mine.decision == Decision::None && a.approvals_required > 0 {
                mine.decision = Decision::ReviewRequired;
            }
        }
        Err(e) => mine.unread.push(format!("the approvals ({e})")),
    }
    match discussions {
        Ok((list, cut)) => {
            let open: Vec<&GlDiscussion> = list.iter().filter(|d| d.notes.iter().any(|n| n.resolvable && !n.resolved)).collect();
            mine.threads_open = open.len() as u32;
            let last_is_theirs =
                |d: &GlDiscussion| d.notes.iter().rfind(|n| !n.system).is_some_and(|n| !n.author.username.eq_ignore_ascii_case(&mine.change.author));
            mine.threads_waiting = open.iter().filter(|d| last_is_theirs(d)).count() as u32;
            if cut {
                mine.unread.push("the discussions past the first 1000".into());
            }
        }
        Err(e) => mine.unread.push(format!("the threads ({e})")),
    }
    match size.map_err(|_| crate::ForgeError::from("stopped")).and_then(|s| s) {
        Ok((a, d)) => (mine.additions, mine.deletions) = (Some(a), Some(d)),
        Err(e) => mine.unread.push(format!("the size ({e})")),
    }
}

/// Lines added and removed, from the change's diff.
fn diff_size(change: &ChangeRef) -> Result<(u64, u64), crate::ForgeError> {
    let files = pr_craft_core::diff::parse_unified(&crate::review::diff(change)?);
    Ok(files.iter().fold((0, 0), |(a, d), f| (a + u64::from(f.additions), d + u64::from(f.deletions))))
}

fn gitlab_list(host: &str) -> Result<(Vec<Mine>, bool), crate::ForgeError> {
    let (mrs, cut) = pages_up_to::<GlMr>(host, "merge_requests?state=opened&scope=created_by_me&order_by=updated_at&", 1)?;
    let mut rows: Vec<(Mine, u32)> = mrs.into_iter().map(|mr| (mr.user_notes_count, gitlab_mine(mr, host))).map(|(n, m)| (m, n)).collect();
    for batch in rows.chunks_mut(6) {
        thread::scope(|scope| {
            for (mine, notes) in batch.iter_mut() {
                let notes = *notes;
                scope.spawn(move || gitlab_fill(mine, notes));
            }
        });
    }
    Ok((rows.into_iter().map(|(m, _)| m).collect(), cut))
}

/// One change read in full: its row, every check, the merge queue and the methods the repository allows. Blocks on the CLI.
pub fn detail_on(change: &ChangeRef) -> Result<MineDetail, crate::ForgeError> {
    match change.forge {
        Forge::GitHub => github_detail(change),
        Forge::GitLab => gitlab_detail(change),
    }
}

fn github_detail(change: &ChangeRef) -> Result<MineDetail, crate::ForgeError> {
    let (owner, name) = change.repo.split_once('/').ok_or_else(|| format!("{} is not `owner/name`", change.repo))?;
    let host = change.host_name();
    let query = format!(
        "query($owner: String!, $name: String!, $number: Int!) {{ repository(owner: $owner, name: $name) {{
           squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed pullRequest(number: $number) {{ ...PrFields }} }} }} {PR_FIELDS}"
    );
    let fields = [("-f", format!("owner={owner}")), ("-f", format!("name={name}")), ("-F", format!("number={}", change.number))];
    let (read, checks) = thread::scope(|scope| {
        let checks = scope.spawn(|| checks_on(change));
        (graphql(host, &query, &fields), checks.join().unwrap_or_else(|_| Err("the checks read stopped".into())))
    });
    let repo = from_json::<Answer<RepoData>>(&read?)?.data()?.repository.ok_or("GitHub did not return the repository")?;
    let methods: Vec<MergeMethod> = [
        (repo.squash_merge_allowed, MergeMethod::Squash),
        (repo.merge_commit_allowed, MergeMethod::Merge),
        (repo.rebase_merge_allowed, MergeMethod::Rebase),
    ]
    .into_iter()
    .filter_map(|(allowed, method)| allowed.then_some(method))
    .collect();
    let pr = repo.pull_request.ok_or("GitHub did not return the change: it is not visible to this login")?;
    let issues: Vec<LinkedIssue> =
        pr.closing_issues_references.nodes.iter().flatten().map(|i| LinkedIssue { number: i.number, title: i.title.clone() }).collect();
    let mut mine = github_mine(pr, host);
    let (checks, checks_error) = match checks {
        Ok(list) => (list, None),
        Err(e) => (Vec::new(), Some(e.message)),
    };
    if checks_error.is_none() && !checks.is_empty() {
        mine.checks = Checks::of(&checks);
    }
    let (queue, queue_error) = match github_queue(host, owner, name, &mine) {
        Ok(queue) => (queue, None),
        Err(e) => (None, Some(e.message)),
    };
    let message = format!("{} (#{})", mine.change.title, mine.change.number);
    Ok(MineDetail { mine, checks, checks_error, queue, closes: issues, methods, message, queue_error })
}

/// The merge queue of the base branch, `None` when the forge says it has none.
fn github_queue(host: &str, owner: &str, name: &str, mine: &Mine) -> Result<Option<MergeQueue>, crate::ForgeError> {
    let query = "query($owner: String!, $name: String!, $branch: String!) { repository(owner: $owner, name: $name) {
        mergeQueue(branch: $branch) { entries(first: 50) { totalCount nodes { estimatedTimeToMerge pullRequest { number } } } } } }";
    let fields = [("-f", format!("owner={owner}")), ("-f", format!("name={name}")), ("-f", format!("branch={}", mine.base_branch))];
    let data = from_json::<Answer<QueueData>>(&graphql(host, query, &fields)?)?.data()?;
    Ok(data.repository.and_then(|r| r.merge_queue).map(|q| queue_of(q, mine.change.number)))
}

fn queue_of(queue: GqlQueue, number: u64) -> MergeQueue {
    let total = queue.entries.total_count.unwrap_or(0);
    let entries: Vec<QueueEntry> = queue.entries.items().collect();
    let queued = entries.iter().any(|e| e.pull_request.as_ref().is_some_and(|p| p.number == number));
    let estimate_secs = entries.iter().rev().find_map(|e| e.estimated_time_to_merge);
    MergeQueue { ahead: if queued { total.saturating_sub(1) } else { total }, estimate_secs, queued }
}

fn gitlab_detail(change: &ChangeRef) -> Result<MineDetail, crate::ForgeError> {
    let (host, base) = (change.host_name(), mr_base(change));
    let mr = from_json::<GlMr>(&run_on(host, "glab", &["api", &base])?)?;
    let notes = mr.user_notes_count;
    let mut mine = gitlab_mine(mr, host);
    let (commits, checks) = thread::scope(|scope| {
        let commits = scope.spawn(|| pages_up_to::<IgnoredAny>(host, &format!("{base}/commits?"), 1));
        let checks = scope.spawn(|| checks_on(change));
        gitlab_fill(&mut mine, notes);
        (commits.join().unwrap_or_else(|_| Err("stopped".into())), checks.join().unwrap_or_else(|_| Err("the checks read stopped".into())))
    });
    match commits {
        Ok((list, false)) => mine.commits = Some(list.len() as u32),
        Ok((_, true)) => mine.unread.push("the commit count (more than 100)".into()),
        Err(e) => mine.unread.push(format!("the commit count ({e})")),
    }
    let (checks, checks_error) = match checks {
        Ok(list) => (list, None),
        Err(e) => (Vec::new(), Some(e.message)),
    };
    if checks_error.is_none() && !checks.is_empty() {
        mine.checks = Checks::of(&checks);
    }
    let message = format!("{}\n", mine.change.title);
    // GitLab reads no merge queue (merge trains need a paid tier and a separate call), so none is reported.
    Ok(MineDetail {
        mine,
        checks,
        checks_error,
        queue: None,
        closes: Vec::new(),
        methods: vec![MergeMethod::Squash, MergeMethod::Merge],
        message,
        queue_error: None,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhCheck {
    name: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    workflow: String,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

/// `<run>/<job>` from the link of a GitHub Actions check of `repo` (`…/<repo>/actions/runs/<run>/job/<job>`). A link into another
/// repository's runs is not this change's job, so it gets no id.
fn actions_id(link: &str, repo: &str) -> Option<String> {
    let digits = |text: &str| -> String { text.chars().take_while(char::is_ascii_digit).collect() };
    let (head, rest) = link.split_once("/actions/runs/")?;
    if !head.ends_with(&format!("/{repo}")) {
        return None;
    }
    let (run, job) = rest.split_once("/job/")?;
    let (run, job) = (digits(run), digits(job));
    (!run.is_empty() && !job.is_empty()).then(|| format!("{run}/{job}"))
}

fn parse_gh_checks(bytes: &[u8], repo: &str) -> Result<Vec<Check>, crate::ForgeError> {
    let list: Vec<GhCheck> = from_json(bytes)?;
    Ok(list
        .into_iter()
        .map(|c| {
            let id = actions_id(&c.link, repo);
            Check {
                has_log: id.is_some(),
                id: id.unwrap_or_else(|| c.name.clone()),
                state: match c.bucket.as_str() {
                    "pass" => CheckState::Passing,
                    "fail" | "cancel" => CheckState::Failing,
                    "skipping" => CheckState::Skipped,
                    _ => CheckState::Running,
                },
                seconds: match (c.started_at, c.completed_at) {
                    (Some(start), Some(end)) => u64::try_from((end - start).num_seconds()).ok().filter(|s| (1..86_400 * 7).contains(s)),
                    _ => None,
                },
                group: (!c.workflow.is_empty()).then_some(c.workflow),
                url: (!c.link.is_empty()).then_some(c.link),
                name: c.name,
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct GlJob {
    id: u64,
    name: String,
    status: String,
    #[serde(default)]
    stage: String,
    duration: Option<f64>,
    web_url: Option<String>,
    #[serde(default)]
    allow_failure: bool,
}

#[derive(Deserialize)]
struct GlHead {
    head_pipeline: Option<GlPipelineId>,
}

#[derive(Deserialize)]
struct GlPipelineId {
    id: u64,
}

fn gitlab_check(job: GlJob) -> Check {
    let state = match job.status.as_str() {
        "success" => CheckState::Passing,
        "failed" if job.allow_failure => CheckState::Skipped,
        "failed" | "canceled" => CheckState::Failing,
        "skipped" | "manual" => CheckState::Skipped,
        _ => CheckState::Running,
    };
    Check {
        id: job.id.to_string(),
        name: job.name,
        group: (!job.stage.is_empty()).then_some(job.stage),
        state,
        seconds: job.duration.filter(|d| *d >= 0.0).map(|d| d as u64),
        url: job.web_url,
        has_log: true,
    }
}

/// Every check of the change at its head: GitHub's check runs and statuses, GitLab's jobs of the head pipeline. A change with no
/// checks gives an empty list. Blocks on the CLI.
pub fn checks_on(change: &ChangeRef) -> Result<Vec<Check>, crate::ForgeError> {
    let host = change.host_name();
    match change.forge {
        Forge::GitHub => {
            let number = change.number.to_string();
            let fields = "name,bucket,link,workflow,startedAt,completedAt";
            match run_on(host, "gh", &["pr", "checks", &number, "-R", &change.repo, "--json", fields]) {
                Ok(bytes) => parse_gh_checks(&bytes, &change.repo),
                Err(e) if e.message.contains("no checks reported") => Ok(Vec::new()),
                Err(e) => Err(e),
            }
        }
        Forge::GitLab => {
            let single: GlHead = from_json(&run_on(host, "glab", &["api", &mr_base(change)])?)?;
            let Some(pipeline) = single.head_pipeline else { return Ok(Vec::new()) };
            let (jobs, cut) = pages::<GlJob>(host, &format!("projects/{}/pipelines/{}/jobs?", project_path(&change.repo), pipeline.id))?;
            if cut {
                return Err("The pipeline has more than 1000 jobs, so its checks were not all read".into());
            }
            Ok(jobs.into_iter().map(gitlab_check).collect())
        }
    }
}

/// The most bytes of a log kept: its end, where the failure is.
const LOG_BYTES: usize = 2 << 20;

/// Runs the CLI and keeps only the last [`LOG_BYTES`] of its output, and whether anything before them was dropped. The whole log is
/// never held: a huge one is read through and forgotten.
fn tail_on(host: &str, cli: &str, args: &[String]) -> Result<(Vec<u8>, bool), crate::ForgeError> {
    let mut child = crate::command(host, cli)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{cli} could not run: {e}"))?;
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(format!("{cli} gave no output to read").into());
    };
    let deadline = crate::bounded::watch(&child, crate::CALL_LIMIT);
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        String::from_utf8_lossy(&text).into_owned()
    });
    let (mut tail, mut cut, mut chunk) = (Vec::new(), false, [0u8; 16 * 1024]);
    let read = loop {
        match stdout.read(&mut chunk) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > 2 * LOG_BYTES {
                    tail.drain(..tail.len() - LOG_BYTES);
                    cut = true;
                }
            }
            Err(e) => break Err(e),
        }
    };
    if tail.len() > LOG_BYTES {
        tail.drain(..tail.len() - LOG_BYTES);
        cut = true;
    }
    let status = child.wait().map_err(|e| format!("{cli} could not run: {e}"))?;
    if deadline.fired() {
        return Err(format!("{cli} did not answer in {} seconds and was stopped", crate::CALL_LIMIT.as_secs()).into());
    }
    read.map_err(|e| format!("could not read {cli}'s output: {e}"))?;
    let stderr = errors.join().unwrap_or_default();
    if status.success() {
        Ok((tail, cut))
    } else {
        Err(crate::ForgeError::exited(format!("`{cli} {}` failed: {}", args.join(" "), stderr.trim()), &stderr))
    }
}

/// The log of one check, cut to its last lines. `all` asks for the whole job instead of only its failed steps (GitHub); GitLab
/// keeps one trace per job. Blocks on the CLI.
pub fn log_on(change: &ChangeRef, check: &str, all: bool) -> Result<JobLog, crate::ForgeError> {
    let host = change.host_name();
    let numeric = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let strings = |args: &[&str]| args.iter().map(|a| (*a).to_string()).collect::<Vec<String>>();
    match change.forge {
        Forge::GitHub => {
            let (run, job) = check
                .split_once('/')
                .filter(|(r, j)| numeric(r) && numeric(j))
                .ok_or("This check is not a GitHub Actions job, so it has no log to read here")?;
            let which = if all { "--log" } else { "--log-failed" };
            let (bytes, cut) = tail_on(host, "gh", &strings(&["run", "view", run, "-R", &change.repo, which, "--job", job]))?;
            Ok(parse_log_cut(&String::from_utf8_lossy(&bytes), true, cut))
        }
        Forge::GitLab => {
            if !numeric(check) {
                return Err(format!("{check:?} is not a GitLab job id").into());
            }
            let path = format!("projects/{}/jobs/{check}/trace", project_path(&change.repo));
            let (bytes, cut) = tail_on(host, "glab", &strings(&["api", &path]))?;
            Ok(parse_log_cut(&String::from_utf8_lossy(&bytes), false, cut))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures: invented names, shaped like `gh api graphql` output of PR_FIELDS.
    const PR: &str = r#"{"number":490,"title":"Use crypto/rand for retry jitter","url":"https://github.com/acme/api/pull/490",
        "updatedAt":"2026-11-17T03:34:10Z","isDraft":false,"additions":9,"deletions":3,"headRefName":"maria/jitter","headRefOid":"7c2e1f0",
        "baseRefName":"main","mergeable":"MERGEABLE","mergeStateStatus":"BLOCKED","reviewDecision":"CHANGES_REQUESTED",
        "repository":{"nameWithOwner":"acme/api"},"author":{"login":"maria"},"commits":{"totalCount":2},
        "reviewRequests":{"nodes":[{"requestedReviewer":{"login":"nina.k"}},{"requestedReviewer":{"slug":"platform"}},{"requestedReviewer":{}}]},
        "latestOpinionatedReviews":{"nodes":[{"state":"CHANGES_REQUESTED","submittedAt":"2026-11-17T00:30:00Z","author":{"login":"j.park"}},
            {"state":"APPROVED","submittedAt":"2026-11-16T00:30:00Z","author":{"login":"nina.k"}}]},
        "reviewThreads":{"totalCount":3,"nodes":[
            {"isResolved":false,"comments":{"nodes":[{"author":{"login":"j.park"}}]}},
            {"isResolved":false,"comments":{"nodes":[{"author":{"login":"maria"}}]}},
            {"isResolved":true,"comments":{"nodes":[{"author":{"login":"j.park"}}]}}]},
        "autoMergeRequest":null,"baseRef":{"branchProtectionRule":{"requiredApprovingReviewCount":2}},
        "labels":{"nodes":[{"name":"bug"}]},"closingIssuesReferences":{"nodes":[{"number":471,"title":"Retries bunch"}]},
        "head":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE","contexts":{"totalCount":14,"nodes":[
            {"__typename":"CheckRun","name":"test:unit","status":"COMPLETED","conclusion":"FAILURE"},
            {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS"},
            {"__typename":"CheckRun","name":"build","status":"IN_PROGRESS","conclusion":null},
            {"__typename":"StatusContext","context":"ci/legacy","state":"SUCCESS"},
            {"__typename":"SomethingNew"}]}}}}]}}"#;

    fn list(pr: &str, more: bool) -> String {
        format!(r#"{{"data":{{"search":{{"pageInfo":{{"hasNextPage":{more}}},"nodes":[{pr},null]}}}}}}"#)
    }

    #[test]
    fn a_github_change_becomes_a_row_with_its_reviews_threads_and_checks() {
        let (rows, more) = parse_github_list(list(PR, true).as_bytes(), "github.com").expect("the list parses");
        assert!(more, "a second page exists, so the list is partial");
        assert_eq!(rows.len(), 1, "a node the forge hid is not a row");
        let m = &rows[0];
        assert_eq!((m.change.reference().as_str(), m.size(), m.commits), ("acme/api #490", Some((9, 3)), Some(2)));
        assert_eq!(m.requested, ["nina.k", "platform"], "a team is named by its slug and an empty reviewer is dropped");
        assert_eq!(m.reviewers, ["nina.k", "platform", "j.park"]);
        assert_eq!((m.decision, m.approvals, m.approvals_required), (Decision::ChangesRequested, 1, Some(2)));
        assert_eq!(m.changes_requested.first().map(|c| c.by.as_str()), Some("j.park"));
        assert_eq!((m.threads_open, m.threads_waiting), (2, 1), "only the thread that ends with the reviewer waits on the author");
        assert_eq!((m.checks.state, m.checks.total, m.checks.failed, m.checks.running), (Some(CheckState::Failing), 14, 1, 1));
        assert_eq!(m.checks.failed_names, ["test:unit"]);
        assert_eq!(m.merge, MergeState::Blocked);
        assert!(m.waiting_on_you());
        assert_eq!(m.change.host, None, "github.com stores no host");
    }

    #[test]
    fn a_hidden_or_failed_answer_is_an_error_not_an_empty_list() {
        assert!(parse_github_list(br#"{"errors":[{"message":"rate limited"}]}"#, "github.com").is_err());
        assert!(parse_github_list(b"[]", "github.com").is_err());
        let ghe = parse_github_list(list(PR, false).as_bytes(), "ghe.acme.dev").expect("parses").0;
        assert_eq!(ghe[0].change.change_ref().host_name(), "ghe.acme.dev");
    }

    #[test]
    fn github_merge_states_map_to_what_the_user_can_do() {
        let with = |mergeable: &str, state: &str| {
            let pr = PR.replace("\"MERGEABLE\"", &format!("\"{mergeable}\"")).replace("\"BLOCKED\"", &format!("\"{state}\""));
            parse_github_list(list(&pr, false).as_bytes(), "github.com").expect("parses").0.remove(0).merge
        };
        assert_eq!(with("CONFLICTING", "DIRTY"), MergeState::Conflicts);
        assert_eq!(with("MERGEABLE", "BEHIND"), MergeState::Behind);
        assert_eq!(with("MERGEABLE", "CLEAN"), MergeState::Ready);
        assert_eq!(with("UNKNOWN", "UNKNOWN"), MergeState::Unknown, "a merge state the forge has not worked out is not Ready");
    }

    #[test]
    fn a_github_checks_list_keeps_the_run_and_job_a_log_is_read_by() {
        let json = br#"[{"bucket":"fail","completedAt":"2026-10-21T11:03:37Z","link":"https://github.com/o/r/actions/runs/39587948030/job/119181432015",
            "name":"Validate","startedAt":"2026-10-21T10:54:02Z","workflow":"PR"},
            {"bucket":"pending","completedAt":"0001-01-01T00:00:00Z","link":"https://example.com/status/1","name":"ci/legacy","startedAt":"0001-01-01T00:00:00Z","workflow":""},
            {"bucket":"skipping","link":"","name":"docs","workflow":"PR"}]"#;
        let checks = parse_gh_checks(json, "o/r").expect("parses");
        assert_eq!(
            (checks[0].id.as_str(), checks[0].state, checks[0].has_log, checks[0].took().as_deref()),
            ("39587948030/119181432015", CheckState::Failing, true, Some("9m 35s"))
        );
        assert_eq!(
            (checks[1].state, checks[1].has_log, checks[1].seconds),
            (CheckState::Running, false, None),
            "an external status has no log to read"
        );
        assert_eq!(checks[2].state, CheckState::Skipped);
        assert!(parse_gh_checks(b"{}", "o/r").is_err());
    }

    #[test]
    fn a_gitlab_change_reads_its_conflicts_and_pipeline() {
        let json =
            br#"{"iid":419,"title":"Saved toast","web_url":"https://gitlab.acme.dev/g/p/-/merge_requests/419","updated_at":"2026-11-17T03:34:10.1Z",
            "author":{"username":"maria"},"references":{"full":"g/sub/p!419"},"source_branch":"feat/x","target_branch":"main","sha":"abc",
            "detailed_merge_status":"discussions_not_resolved","has_conflicts":false,"reviewers":[{"username":"nina.k"}],"labels":["bug"],
            "user_notes_count":3,"head_pipeline":{"id":2020,"status":"failed"}}"#;
        let m = gitlab_mine(from_json(json).expect("parses"), "gitlab.acme.dev");
        assert_eq!((m.change.repo.as_str(), m.change.number, m.change.host.as_deref()), ("g/sub/p", 419, Some("gitlab.acme.dev")));
        assert_eq!((m.merge, m.merge_note.as_deref()), (MergeState::Blocked, Some("discussions not resolved")));
        assert_eq!(m.checks.state, Some(CheckState::Failing));
        assert_eq!(m.size(), None, "the size is unknown until its own read answers");
        let conflict = json.iter().map(|b| *b as char).collect::<String>().replace("\"has_conflicts\":false", "\"has_conflicts\":true");
        assert_eq!(gitlab_mine(from_json(conflict.as_bytes()).expect("parses"), "gitlab.acme.dev").merge, MergeState::Conflicts);
    }

    #[test]
    fn a_gitlab_job_that_may_fail_does_not_make_the_checks_fail() {
        let job = |status: &str, allow: bool| GlJob {
            id: 7,
            name: "lint".into(),
            status: status.into(),
            stage: "test".into(),
            duration: Some(41.7),
            web_url: None,
            allow_failure: allow,
        };
        assert_eq!(gitlab_check(job("failed", false)).state, CheckState::Failing);
        assert_eq!(gitlab_check(job("failed", true)).state, CheckState::Skipped);
        assert_eq!(gitlab_check(job("running", false)).state, CheckState::Running);
        assert_eq!(gitlab_check(job("success", false)).took().as_deref(), Some("41s"));
    }

    #[test]
    fn a_queue_counts_the_changes_ahead_and_knows_when_the_change_is_in_it() {
        let queue = |json: &str| queue_of(from_json::<GqlQueue>(json.as_bytes()).expect("parses"), 490);
        let q =
            queue(r#"{"entries":{"totalCount":3,"nodes":[{"estimatedTimeToMerge":480,"pullRequest":{"number":1}},{"pullRequest":{"number":2}}]}}"#);
        assert_eq!((q.ahead, q.queued, q.estimate_secs), (3, false, Some(480)));
        let q = queue(r#"{"entries":{"totalCount":3,"nodes":[{"pullRequest":{"number":490}}]}}"#);
        assert_eq!((q.ahead, q.queued), (2, true));
    }

    #[test]
    fn a_log_is_read_only_for_a_job_the_forge_can_name() {
        let change = ChangeRef { forge: Forge::GitHub, repo: "o/r".into(), number: 1, host: None };
        assert!(log_on(&change, "ci/legacy", false).expect_err("not an Actions job").message.contains("no log"));
        assert!(log_on(&change, "12/--help", false).is_err(), "an id is digits, never an option for gh");
        let gitlab = ChangeRef { forge: Forge::GitLab, ..change };
        assert!(log_on(&gitlab, "../x", false).is_err());
    }

    #[test]
    fn a_check_that_points_into_another_repositorys_runs_is_not_this_changes_job() {
        let own = "https://github.com/acme/api/actions/runs/5/job/6";
        assert_eq!(actions_id(own, "acme/api").as_deref(), Some("5/6"));
        assert_eq!(actions_id("https://github.com/acme/other/actions/runs/5/job/6", "acme/api"), None, "another repository's run");
        assert_eq!(
            actions_id("https://evil.dev/x/acme/api/actions/runs/5/job/6", "acme/api").as_deref(),
            Some("5/6"),
            "the host is checked where it is opened"
        );
    }
}
