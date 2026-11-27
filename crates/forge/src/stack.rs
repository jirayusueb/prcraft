//! Reads what stacking needs: a repository's open changes with their branches, and the diff between two branches.

use pr_craft_core::stack::StackLayer;
use pr_craft_core::{ChangeRef, Forge};
use serde::Deserialize;

use crate::review::project_path;
use crate::{from_json, run_on};

/// The most open changes read per repository.
pub fn layer_limit(forge: Forge) -> usize {
    match forge {
        Forge::GitHub => 200,
        Forge::GitLab => 100,
    }
}

/// Whether `count` open changes fill the page, so more may exist and a stack built from them may be incomplete.
pub fn is_full_page(forge: Forge, count: usize) -> bool {
    count >= layer_limit(forge)
}

/// Every open change of `repo` on `host` with its head and base branch. Blocks on the CLI; call it off the UI thread.
// shortcut: one page per repository; walk pages when a repository keeps more open changes than `layer_limit`.
pub fn open_layers(forge: Forge, host: &str, repo: &str) -> Result<Vec<StackLayer>, String> {
    match forge {
        Forge::GitHub => {
            let limit = layer_limit(forge).to_string();
            let fields = "number,title,author,headRefName,baseRefName,isDraft,isCrossRepository,url";
            parse_github_layers(&run_on(host, "gh", &["pr", "list", "-R", repo, "--state", "open", "--limit", &limit, "--json", fields])?, host, repo)
        }
        Forge::GitLab => {
            let query = format!("projects/{}/merge_requests?state=opened&per_page={}", project_path(repo), layer_limit(forge));
            parse_gitlab_layers(&run_on(host, "glab", &["api", &query])?, host, repo)
        }
    }
}

/// A branch comparison as git unified text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compare {
    pub text: String,
    /// Files the forge listed without their lines because they are too large. `text` holds them as empty files.
    pub omitted: Vec<String>,
    /// Where the forge shows this comparison, for the files it left out.
    pub web_url: Option<String>,
}

/// The unified diff of `head` against its merge base with `base`: what a stack adds, or what one layer adds.
/// Blocks on the CLI; call it off the UI thread.
pub fn compare_diff(change: &ChangeRef, base: &str, head: &str) -> Result<Compare, String> {
    match change.forge {
        Forge::GitHub => {
            // GitHub reads a slash inside a ref in the path; every other special byte is escaped.
            let path = format!("repos/{}/compare/{}...{}", change.repo, encode(base).replace("%2F", "/"), encode(head).replace("%2F", "/"));
            let bytes = run_on(change.host_name(), "gh", &["api", "-H", "Accept: application/vnd.github.diff", &path])?;
            let web_url = format!(
                "https://{}/{}/compare/{}...{}",
                change.host_name(),
                change.repo,
                encode(base).replace("%2F", "/"),
                encode(head).replace("%2F", "/")
            );
            Ok(Compare { text: String::from_utf8_lossy(&bytes).into_owned(), omitted: Vec::new(), web_url: Some(web_url) })
        }
        Forge::GitLab => {
            let path = format!("projects/{}/repository/compare?from={}&to={}", project_path(&change.repo), encode(base), encode(head));
            gitlab_compare_to_unified(&run_on(change.host_name(), "glab", &["api", &path])?)
        }
    }
}

/// Percent-encodes every byte outside RFC 3986's unreserved set, so a branch such as `feat/a&b` stays one value.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') { char::from(b).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// A person as either forge names one: `login` on GitHub, `username` on GitLab.
#[derive(Deserialize)]
struct Person {
    #[serde(alias = "username")]
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubLayer {
    number: u64,
    title: String,
    author: Option<Person>,
    head_ref_name: String,
    base_ref_name: String,
    is_draft: bool,
    /// The head branch is in a fork.
    is_cross_repository: bool,
    url: String,
}

fn parse_github_layers(bytes: &[u8], host: &str, repo: &str) -> Result<Vec<StackLayer>, String> {
    let prs: Vec<GitHubLayer> = from_json(bytes)?;
    Ok(prs
        .into_iter()
        .map(|pr| StackLayer {
            change: ChangeRef { forge: Forge::GitHub, repo: repo.to_string(), number: pr.number, host: Forge::GitHub.host_field(host) },
            title: pr.title,
            author: pr.author.map(|a| a.login).unwrap_or_default(),
            head_branch: pr.head_ref_name,
            base_branch: pr.base_ref_name,
            draft: pr.is_draft,
            url: pr.url,
            head_in_repo: !pr.is_cross_repository,
        })
        .collect())
}

#[derive(Deserialize)]
struct GitLabLayer {
    iid: u64,
    title: String,
    author: Option<Person>,
    source_branch: String,
    target_branch: String,
    /// `null` once the source project is deleted.
    source_project_id: Option<u64>,
    target_project_id: Option<u64>,
    #[serde(default)]
    draft: bool,
    /// What older GitLab servers send instead of `draft`.
    #[serde(default)]
    work_in_progress: bool,
    web_url: String,
}

fn parse_gitlab_layers(bytes: &[u8], host: &str, repo: &str) -> Result<Vec<StackLayer>, String> {
    let mrs: Vec<GitLabLayer> = from_json(bytes)?;
    Ok(mrs
        .into_iter()
        .map(|mr| StackLayer {
            change: ChangeRef { forge: Forge::GitLab, repo: repo.to_string(), number: mr.iid, host: Forge::GitLab.host_field(host) },
            title: mr.title,
            author: mr.author.map(|a| a.login).unwrap_or_default(),
            head_branch: mr.source_branch,
            base_branch: mr.target_branch,
            draft: mr.draft || mr.work_in_progress,
            url: mr.web_url,
            head_in_repo: mr.source_project_id.is_some() && mr.source_project_id == mr.target_project_id,
        })
        .collect())
}

#[derive(Deserialize)]
struct GitLabCompare {
    web_url: Option<String>,
    /// GitLab gave up comparing; `diffs` is then partial or empty.
    #[serde(default)]
    compare_timeout: bool,
    diffs: Vec<GitLabFileDiff>,
}

#[derive(Deserialize)]
struct GitLabFileDiff {
    old_path: String,
    new_path: String,
    #[serde(default)]
    new_file: bool,
    #[serde(default)]
    deleted_file: bool,
    #[serde(default)]
    renamed_file: bool,
    /// GitLab left the lines out because the diff is large.
    #[serde(default)]
    collapsed: bool,
    #[serde(default)]
    too_large: bool,
    /// Hunks only: GitLab sends no `diff --git` or `---`/`+++` lines.
    #[serde(default)]
    diff: String,
}

/// GitLab's compare answer as the git unified text `parse_unified` reads.
fn gitlab_compare_to_unified(bytes: &[u8]) -> Result<Compare, String> {
    let compare: GitLabCompare = from_json(bytes)?;
    if compare.compare_timeout {
        return Err("GitLab timed out comparing these branches, so it sent an incomplete diff. Open the stack on GitLab instead.".into());
    }
    let mut text = String::new();
    let mut omitted = Vec::new();
    for file in compare.diffs {
        let (old, new) = (&file.old_path, &file.new_path);
        text.push_str(&format!("diff --git a/{old} b/{new}\n"));
        if file.new_file {
            text.push_str("new file mode 100644\n");
        } else if file.deleted_file {
            text.push_str("deleted file mode 100644\n");
        } else if file.renamed_file {
            text.push_str(&format!("rename from {old}\nrename to {new}\n"));
        }
        if file.diff.is_empty() {
            if file.collapsed || file.too_large {
                omitted.push(if file.deleted_file { old.clone() } else { new.clone() });
            }
            continue;
        }
        let from = if file.new_file { "/dev/null".to_string() } else { format!("a/{old}") };
        let to = if file.deleted_file { "/dev/null".to_string() } else { format!("b/{new}") };
        text.push_str(&format!("--- {from}\n+++ {to}\n{}", file.diff));
        if !file.diff.ends_with('\n') {
            text.push('\n');
        }
    }
    Ok(Compare { text, omitted, web_url: compare.web_url })
}

#[cfg(test)]
mod tests {
    use pr_craft_core::diff::{FileStatus, parse_unified};

    use super::*;

    #[test]
    fn github_layers_carry_branches_and_a_missing_author_is_blank() {
        // Shaped like `gh pr list --json number,title,author,headRefName,baseRefName,isDraft,isCrossRepository,url`; names invented.
        let json = br#"[
            {"number":7,"title":"Add the table","author":{"login":"ada"},"headRefName":"ada/table","baseRefName":"main","isDraft":false,"isCrossRepository":false,"url":"https://github.com/acme/api/pull/7"},
            {"number":8,"title":"Use the table","author":null,"headRefName":"ada/use","baseRefName":"ada/table","isDraft":true,"isCrossRepository":true,"url":"https://github.com/acme/api/pull/8"}]"#;
        let layers = parse_github_layers(json, "github.com", "acme/api").expect("parses");
        assert_eq!(layers.len(), 2);
        assert_eq!((layers[0].head_branch.as_str(), layers[0].base_branch.as_str()), ("ada/table", "main"));
        assert_eq!(layers[0].change, ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 7, host: None });
        assert_eq!((layers[1].author.as_str(), layers[1].draft), ("", true));
        assert_eq!((layers[0].head_in_repo, layers[1].head_in_repo), (true, false));
    }

    #[test]
    fn gitlab_forks_and_deleted_sources_are_not_in_the_repository() {
        // Shaped like GET /projects/:id/merge_requests; ids invented. The second MR's source project was deleted.
        let json = br#"[
            {"iid":1,"title":"a","author":{"username":"u"},"source_branch":"x","target_branch":"main","source_project_id":5,"target_project_id":7,"web_url":"u1"},
            {"iid":2,"title":"b","author":{"username":"u"},"source_branch":"y","target_branch":"main","source_project_id":null,"target_project_id":7,"web_url":"u2"},
            {"iid":3,"title":"c","author":{"username":"u"},"source_branch":"z","target_branch":"main","source_project_id":7,"target_project_id":7,"web_url":"u3"}]"#;
        let layers = parse_gitlab_layers(json, "gitlab.com", "acme/billing").expect("parses");
        assert_eq!(layers.iter().map(|l| l.head_in_repo).collect::<Vec<_>>(), [false, false, true]);
    }

    #[test]
    fn a_full_page_is_flagged() {
        for forge in Forge::ALL {
            let limit = layer_limit(forge);
            assert!(!is_full_page(forge, limit - 1), "{forge:?} one short of the limit");
            assert!(is_full_page(forge, limit), "{forge:?} at the limit");
        }
    }

    #[test]
    fn a_gitlab_compare_timeout_is_an_error() {
        // GitLab sends `compare_timeout: true` with whatever diffs it finished.
        let json = br#"{"compare_timeout":true,"diffs":[{"old_path":"a","new_path":"a","diff":"@@ -1 +1 @@\n-a\n+b\n"}]}"#;
        assert!(gitlab_compare_to_unified(json).is_err());
    }

    #[test]
    fn gitlab_files_left_out_for_size_are_named() {
        // Shaped like compare `diffs[]` entries GitLab collapsed or marked too large; paths invented.
        let json = br#"{"compare_timeout":false,"web_url":"https://gitlab.example/acme/billing/-/compare/main...top","diffs":[
            {"old_path":"big.json","new_path":"big.json","collapsed":true,"too_large":false,"diff":""},
            {"old_path":"huge.sql","new_path":"huge.sql","too_large":true,"diff":""},
            {"old_path":"gone.sql","new_path":"gone.sql","deleted_file":true,"too_large":true,"diff":""},
            {"old_path":"mode.sh","new_path":"mode.sh","diff":""},
            {"old_path":"a.txt","new_path":"a.txt","diff":"@@ -1 +1 @@\n-a\n+b\n"}]}"#;
        let compare = gitlab_compare_to_unified(json).expect("converts");
        assert_eq!(compare.omitted, ["big.json", "huge.sql", "gone.sql"]);
        assert_eq!(compare.web_url.as_deref(), Some("https://gitlab.example/acme/billing/-/compare/main...top"));
        let files = parse_unified(&compare.text);
        assert_eq!(files.len(), 5);
        assert!(files[0].hunks.is_empty() && files[4].hunks.len() == 1);
    }

    #[test]
    fn gitlab_layers_read_source_and_target_branches() {
        // Shaped like GET /projects/:id/merge_requests; names invented.
        let json = br#"[{"iid":12,"title":"Split billing","author":{"username":"grace"},"source_branch":"grace/split",
            "target_branch":"main","source_project_id":7,"target_project_id":7,"work_in_progress":true,"web_url":"https://gitlab.example/acme/billing/-/merge_requests/12"}]"#;
        let layers = parse_gitlab_layers(json, "gitlab.com", "acme/billing").expect("parses");
        assert_eq!((layers[0].change.number, layers[0].author.as_str(), layers[0].draft), (12, "grace", true));
        assert_eq!((layers[0].head_branch.as_str(), layers[0].base_branch.as_str()), ("grace/split", "main"));
    }

    #[test]
    fn branch_names_are_escaped_for_a_query() {
        assert_eq!(encode("feat/a&b#1"), "feat%2Fa%26b%231");
        assert_eq!(encode("release-1.2_x~"), "release-1.2_x~");
    }

    #[test]
    fn gitlab_compare_round_trips_through_parse_unified() {
        // Shaped like GET /projects/:id/repository/compare; paths and code invented.
        let json = br#"{"commit":{"id":"abc"},"diffs":[
            {"old_path":"a.txt","new_path":"a.txt","new_file":false,"deleted_file":false,"renamed_file":false,
             "diff":"@@ -1,2 +1,2 @@\n one\n-two\n+three\n"},
            {"old_path":"new.txt","new_path":"new.txt","new_file":true,"deleted_file":false,"renamed_file":false,
             "diff":"@@ -0,0 +1 @@\n+hello"},
            {"old_path":"gone.txt","new_path":"gone.txt","new_file":false,"deleted_file":true,"renamed_file":false,
             "diff":"@@ -1 +0,0 @@\n-bye\n"},
            {"old_path":"old name.txt","new_path":"new name.txt","new_file":false,"deleted_file":false,"renamed_file":true,"diff":""}]}"#;
        let files = parse_unified(&gitlab_compare_to_unified(json).expect("converts").text);
        assert_eq!(files.len(), 4);
        assert_eq!((files[0].path.as_str(), files[0].status, files[0].additions, files[0].deletions), ("a.txt", FileStatus::Modified, 1, 1));
        assert_eq!((files[1].path.as_str(), files[1].status, files[1].additions), ("new.txt", FileStatus::Added, 1));
        assert_eq!((files[2].path.as_str(), files[2].status, files[2].deletions), ("gone.txt", FileStatus::Deleted, 1));
        assert_eq!(files[3].status, FileStatus::Renamed);
        assert_eq!((files[3].path.as_str(), files[3].old_path.as_deref()), ("new name.txt", Some("old name.txt")));
    }

    #[test]
    fn a_compare_without_diffs_is_an_error_not_an_empty_stack() {
        assert!(gitlab_compare_to_unified(b"{\"message\":\"404 Not Found\"}").is_err());
    }
}
