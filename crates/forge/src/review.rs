//! Reads one change's diff and posts a finished review, through `gh` and `glab`.

use pr_craft_core::review::{Draft, LineRef, Verdict};
use pr_craft_core::{ChangeRef, Forge};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{from_json, run_on, run_with_input_on, run_write_on};

/// What the review header shows and what posting needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeMeta {
    pub title: String,
    pub author: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    /// GitHub anchors review comments to this commit.
    pub head_sha: String,
    /// The commit the base side of a file view reads: the merge-base of the base branch and the head, so it is the old
    /// side of the diff. GitHub's compare API names it; GitLab's `diff_refs.base_sha` is already it. `None` when GitLab
    /// sent no diff refs or GitHub's compare call failed.
    pub base_sha: Option<String>,
    /// GitLab anchors discussions to these three; `None` on GitHub.
    pub diff_refs: Option<GitLabDiffRefs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLabDiffRefs {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
}

/// Title, author, branches and the commits comments anchor to. Blocks on the CLI.
pub fn change_meta(change: &ChangeRef) -> Result<ChangeMeta, String> {
    match change.forge {
        Forge::GitHub => {
            let number = change.number.to_string();
            let fields = "title,author,url,headRefName,baseRefName,headRefOid,baseRefOid";
            let host = change.host_name();
            let (mut meta, tip) = parse_github_meta(&run_on(host, "gh", &["pr", "view", &number, "-R", &change.repo, "--json", fields])?)?;
            // The base branch may have moved on since the branch left it; the diff's old side is the merge-base. If
            // GitHub will not say, the review still opens and the base side of a file view explains the gap.
            let path = github_compare_path(&change.repo, &tip, &meta.head_sha);
            meta.base_sha = run_on(host, "gh", &["api", &path, "--jq", ".merge_base_commit.sha"]).and_then(|found| merge_base(&found)).ok();
            Ok(meta)
        }
        Forge::GitLab => parse_gitlab_meta(&run_on(change.host_name(), "glab", &["api", &mr_path(change, "")])?),
    }
}

/// The whole change as one unified diff. Blocks on the CLI.
pub fn diff(change: &ChangeRef) -> Result<String, String> {
    let number = change.number.to_string();
    let bytes = match change.forge {
        Forge::GitHub => run_on(change.host_name(), "gh", &["pr", "diff", &number, "-R", &change.repo])?,
        Forge::GitLab => run_on(change.host_name(), "glab", &["mr", "diff", &number, "-R", &change.repo, "--raw"])?,
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The compare call that names the merge-base of `base` and `head`; one file of the answer is enough.
fn github_compare_path(repo: &str, base: &str, head: &str) -> String {
    format!("repos/{repo}/compare/{base}...{head}?per_page=1")
}

/// The commit id `gh api --jq .merge_base_commit.sha` printed. Anything else is an error, never a guess.
fn merge_base(output: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(output);
    let sha = text.trim();
    if sha.len() >= 7 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha.to_string())
    } else {
        Err(format!("GitHub named no merge-base commit: {sha:?}"))
    }
}

/// Posts every draft, the body and the verdict as one GitHub review.
pub fn github_submit(change: &ChangeRef, meta: &ChangeMeta, verdict: Verdict, body: &str, drafts: &[Draft]) -> Result<(), String> {
    let path = format!("repos/{}/pulls/{}/reviews", change.repo, change.number);
    let input = github_review_json(meta, verdict, body, drafts).to_string();
    run_with_input_on(change.host_name(), "gh", &["api", &path, "--method", "POST", "--input", "-"], input.as_bytes()).map(drop)
}

/// Posts one draft as a GitLab discussion on its line.
pub fn gitlab_post_draft(change: &ChangeRef, meta: &ChangeMeta, draft: &Draft) -> Result<(), String> {
    let input = gitlab_discussion_json(meta, draft)?.to_string();
    gitlab_post(change, &mr_path(change, "/discussions"), &input)
}

/// Posts the general comment as a GitLab note.
pub fn gitlab_post_note(change: &ChangeRef, body: &str) -> Result<(), String> {
    gitlab_post(change, &mr_path(change, "/notes"), &json!({ "body": body }).to_string())
}

/// Approves the merge request.
pub fn gitlab_approve(change: &ChangeRef) -> Result<(), String> {
    run_write_on(change.host_name(), "glab", &["api", &mr_path(change, "/approve"), "--method", "POST"]).map(drop)
}

fn gitlab_post(change: &ChangeRef, path: &str, input: &str) -> Result<(), String> {
    let args = ["api", path, "--method", "POST", "-H", "Content-Type: application/json", "--input", "-"];
    run_with_input_on(change.host_name(), "glab", &args, input.as_bytes()).map(drop)
}

/// `projects/<path with / as %2F>/merge_requests/<iid><tail>`.
fn mr_path(change: &ChangeRef, tail: &str) -> String {
    format!("projects/{}/merge_requests/{}{tail}", project_path(&change.repo), change.number)
}

/// A GitLab project path as one URL segment. GitLab paths are `[A-Za-z0-9_.-/]`, so `/` is the only byte to encode.
pub(crate) fn project_path(repo: &str) -> String {
    repo.replace('/', "%2F")
}

/// GitHub names a line by side and number: the new side when the line has one, else the old.
fn github_line(line: LineRef) -> (u32, &'static str) {
    match line.new {
        Some(new) => (new, "RIGHT"),
        None => (line.old.unwrap_or(0), "LEFT"),
    }
}

fn github_review_json(meta: &ChangeMeta, verdict: Verdict, body: &str, drafts: &[Draft]) -> Value {
    let event = match verdict {
        Verdict::Comment => "COMMENT",
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
    };
    let comments: Vec<Value> = drafts
        .iter()
        .map(|draft| {
            let (line, side) = github_line(draft.end);
            let mut comment = json!({ "path": draft.path, "body": draft.body, "line": line, "side": side });
            if let Some(start) = draft.start.filter(|start| *start != draft.end) {
                let (start_line, start_side) = github_line(start);
                comment["start_line"] = start_line.into();
                comment["start_side"] = start_side.into();
            }
            comment
        })
        .collect();
    let mut review = json!({ "commit_id": meta.head_sha, "event": event, "comments": comments });
    if !(body.is_empty() && verdict == Verdict::Approve) {
        review["body"] = body.into();
    }
    review
}

fn gitlab_discussion_json(meta: &ChangeMeta, draft: &Draft) -> Result<Value, String> {
    let refs = meta.diff_refs.as_ref().ok_or("this merge request has no diff refs, so a comment cannot anchor to a line")?;
    let mut position = json!({
        "position_type": "text",
        "base_sha": refs.base_sha,
        "start_sha": refs.start_sha,
        "head_sha": refs.head_sha,
        "old_path": draft.old_path.as_deref().unwrap_or(&draft.path),
        "new_path": draft.path,
    });
    if let Some(new) = draft.end.new {
        position["new_line"] = new.into();
    }
    if let Some(old) = draft.end.old {
        position["old_line"] = old.into();
    }
    // shortcut: a multi-line comment anchors on its end line with the range in the text (GitLab's line_range needs line codes); use line_range when that matters.
    let body = match draft.start.filter(|start| *start != draft.end) {
        Some(start) => format!("Lines {}–{}:\n\n{}", start.shown(), draft.end.shown(), draft.body),
        None => draft.body.clone(),
    };
    Ok(json!({ "body": body, "position": position }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubMeta {
    title: String,
    author: GitHubAuthor,
    url: String,
    head_ref_name: String,
    base_ref_name: String,
    head_ref_oid: String,
    base_ref_oid: String,
}

#[derive(Deserialize)]
struct GitHubAuthor {
    login: String,
}

/// The meta, with no base commit yet, and the base branch's tip: [`change_meta`] asks GitHub for the merge-base.
fn parse_github_meta(bytes: &[u8]) -> Result<(ChangeMeta, String), String> {
    let pr: GitHubMeta = from_json(bytes)?;
    let meta = ChangeMeta {
        title: pr.title,
        author: pr.author.login,
        url: pr.url,
        head_branch: pr.head_ref_name,
        base_branch: pr.base_ref_name,
        head_sha: pr.head_ref_oid,
        base_sha: None,
        diff_refs: None,
    };
    Ok((meta, pr.base_ref_oid))
}

#[derive(Deserialize)]
struct GitLabMeta {
    title: String,
    author: GitLabAuthor,
    web_url: String,
    source_branch: String,
    target_branch: String,
    sha: String,
    diff_refs: Option<GitLabDiffRefs>,
}

#[derive(Deserialize)]
struct GitLabAuthor {
    username: String,
}

fn parse_gitlab_meta(bytes: &[u8]) -> Result<ChangeMeta, String> {
    let mr: GitLabMeta = from_json(bytes)?;
    Ok(ChangeMeta {
        title: mr.title,
        author: mr.author.username,
        url: mr.web_url,
        head_branch: mr.source_branch,
        base_branch: mr.target_branch,
        head_sha: mr.sha,
        base_sha: mr.diff_refs.as_ref().map(|refs| refs.base_sha.clone()),
        diff_refs: mr.diff_refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(diff_refs: Option<GitLabDiffRefs>) -> ChangeMeta {
        ChangeMeta {
            title: "Retry webhooks".into(),
            author: "nina.k".into(),
            url: "https://example.test/pull/1".into(),
            head_branch: "retry".into(),
            base_branch: "main".into(),
            head_sha: "headsha".into(),
            base_sha: None,
            diff_refs,
        }
    }

    fn refs() -> GitLabDiffRefs {
        GitLabDiffRefs { base_sha: "b".into(), start_sha: "s".into(), head_sha: "h".into() }
    }

    fn draft(start: Option<LineRef>, end: LineRef) -> Draft {
        Draft { id: 1, path: "src/retry.go".into(), old_path: None, start, end, body: "Why 3?".into() }
    }

    const ADDED: LineRef = LineRef { old: None, new: Some(42) };
    const REMOVED: LineRef = LineRef { old: Some(40), new: None };
    const CONTEXT: LineRef = LineRef { old: Some(40), new: Some(42) };

    // Fixtures: invented names, shaped like `gh pr view --json` and GitLab's `GET /merge_requests/:iid` output.
    #[test]
    fn github_view_output_becomes_meta() {
        let json = br#"{"author":{"id":"U_1","is_bot":false,"login":"nina.k","name":"Nina"},"baseRefName":"main",
            "headRefName":"retry-webhooks","headRefOid":"9f2c1a","baseRefOid":"b4a3c2","title":"Retry webhooks","url":"https://github.com/acme/api/pull/482"}"#;
        let (meta, tip) = parse_github_meta(json).expect("gh output parses");
        assert_eq!((meta.author.as_str(), meta.head_branch.as_str(), meta.base_branch.as_str()), ("nina.k", "retry-webhooks", "main"));
        assert_eq!(meta.head_sha, "9f2c1a");
        assert_eq!(tip, "b4a3c2", "the base branch tip is kept to ask for the merge-base");
        assert_eq!(meta.base_sha, None, "the merge-base is not the tip");
        assert_eq!(meta.diff_refs, None);
    }

    #[test]
    fn the_merge_base_comes_from_the_compare_call_and_only_a_commit_id_counts() {
        assert_eq!(github_compare_path("acme/api", "b4a3c2", "9f2c1a"), "repos/acme/api/compare/b4a3c2...9f2c1a?per_page=1");
        assert_eq!(merge_base(b"0a1b2c3d4e5f\n"), Ok("0a1b2c3d4e5f".to_string()));
        for bad in ["", "null\n", "not a sha", "abc"] {
            assert!(merge_base(bad.as_bytes()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn gitlab_mr_output_becomes_meta_with_diff_refs() {
        let json = br#"{"iid":88,"title":"Pin provider","web_url":"https://gitlab.acme.dev/infra/aws/modules/-/merge_requests/88",
            "author":{"username":"r.amini"},"source_branch":"pin","target_branch":"main","sha":"c0ffee",
            "diff_refs":{"base_sha":"aaa","head_sha":"c0ffee","start_sha":"bbb"}}"#;
        let meta = parse_gitlab_meta(json).expect("glab output parses");
        assert_eq!((meta.author.as_str(), meta.head_branch.as_str(), meta.head_sha.as_str()), ("r.amini", "pin", "c0ffee"));
        assert_eq!(meta.diff_refs, Some(GitLabDiffRefs { base_sha: "aaa".into(), start_sha: "bbb".into(), head_sha: "c0ffee".into() }));
        assert_eq!(meta.base_sha.as_deref(), Some("aaa"));
        assert!(parse_gitlab_meta(br#"{"message":"404 Not found"}"#).is_err());
    }

    #[test]
    fn github_side_follows_the_line_that_exists() {
        let review = github_review_json(&meta(None), Verdict::Comment, "", &[draft(None, ADDED), draft(None, REMOVED), draft(None, CONTEXT)]);
        let sides: Vec<_> = review["comments"].as_array().expect("comments").iter().map(|c| (c["line"].as_u64(), c["side"].as_str())).collect();
        assert_eq!(sides, [(Some(42), Some("RIGHT")), (Some(40), Some("LEFT")), (Some(42), Some("RIGHT"))]);
        assert_eq!(review["commit_id"], "headsha");
        assert_eq!(review["event"], "COMMENT");
    }

    #[test]
    fn github_start_line_only_for_a_real_range() {
        let start = LineRef { old: None, new: Some(40) };
        let review =
            github_review_json(&meta(None), Verdict::Comment, "", &[draft(Some(start), ADDED), draft(Some(ADDED), ADDED), draft(None, ADDED)]);
        let comments = review["comments"].as_array().expect("comments");
        assert_eq!((comments[0]["start_line"].as_u64(), comments[0]["start_side"].as_str()), (Some(40), Some("RIGHT")));
        for single in &comments[1..] {
            assert!(single.get("start_line").is_none() && single.get("start_side").is_none(), "{single}");
        }
    }

    #[test]
    fn github_body_is_dropped_only_for_an_empty_approval() {
        let approve = github_review_json(&meta(None), Verdict::Approve, "", &[]);
        assert_eq!(approve["event"], "APPROVE");
        assert!(approve.get("body").is_none());
        assert_eq!(github_review_json(&meta(None), Verdict::Approve, "Nice", &[])["body"], "Nice");
        let changes = github_review_json(&meta(None), Verdict::RequestChanges, "", &[]);
        assert_eq!((changes["event"].as_str(), changes["body"].as_str()), (Some("REQUEST_CHANGES"), Some("")));
    }

    #[test]
    fn gitlab_context_line_sends_both_numbers_and_others_one() {
        let position = |line| gitlab_discussion_json(&meta(Some(refs())), &draft(None, line)).expect("has refs")["position"].clone();
        let context = position(CONTEXT);
        assert_eq!((context["old_line"].as_u64(), context["new_line"].as_u64()), (Some(40), Some(42)));
        let added = position(ADDED);
        assert!(added.get("old_line").is_none() && added["new_line"] == 42, "{added}");
        let removed = position(REMOVED);
        assert!(removed.get("new_line").is_none() && removed["old_line"] == 40, "{removed}");
        assert_eq!((context["base_sha"].as_str(), context["start_sha"].as_str(), context["head_sha"].as_str()), (Some("b"), Some("s"), Some("h")));
        assert_eq!(context["position_type"], "text");
    }

    #[test]
    fn gitlab_old_path_falls_back_to_path_and_survives_renames() {
        let plain = gitlab_discussion_json(&meta(Some(refs())), &draft(None, ADDED)).expect("has refs");
        assert_eq!((plain["position"]["old_path"].as_str(), plain["position"]["new_path"].as_str()), (Some("src/retry.go"), Some("src/retry.go")));
        let renamed = Draft { old_path: Some("src/old.go".into()), ..draft(None, ADDED) };
        let renamed = gitlab_discussion_json(&meta(Some(refs())), &renamed).expect("has refs");
        assert_eq!((renamed["position"]["old_path"].as_str(), renamed["position"]["new_path"].as_str()), (Some("src/old.go"), Some("src/retry.go")));
    }

    #[test]
    fn gitlab_multi_line_anchors_on_the_end_and_names_the_range() {
        let start = LineRef { old: None, new: Some(40) };
        let ranged = gitlab_discussion_json(&meta(Some(refs())), &draft(Some(start), ADDED)).expect("has refs");
        assert_eq!(ranged["body"], "Lines 40–42:\n\nWhy 3?");
        assert_eq!(ranged["position"]["new_line"], 42);
        let single = gitlab_discussion_json(&meta(Some(refs())), &draft(Some(ADDED), ADDED)).expect("has refs");
        assert_eq!(single["body"], "Why 3?");
    }

    #[test]
    fn gitlab_comment_without_diff_refs_is_an_error() {
        let err = gitlab_discussion_json(&meta(None), &draft(None, ADDED)).expect_err("no refs, no anchor");
        assert!(err.contains("diff refs"), "{err}");
    }

    #[test]
    fn gitlab_subgroup_path_is_one_encoded_segment() {
        let change = ChangeRef { forge: Forge::GitLab, repo: "infra/aws/modules".into(), number: 88, host: None };
        assert_eq!(mr_path(&change, "/notes"), "projects/infra%2Faws%2Fmodules/merge_requests/88/notes");
    }
}
