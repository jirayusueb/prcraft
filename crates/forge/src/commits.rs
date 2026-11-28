//! The commits of one change, read through `gh` and `glab`.

use pr_craft_core::commits::Commit;
use pr_craft_core::{ChangeRef, DateTime, Forge, Utc};
use serde::Deserialize;

use crate::review::project_path;
use crate::turn::pages;
use crate::{from_json, run_on};

/// The commits of a change, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRead {
    pub commits: Vec<Commit>,
    /// Every commit was read.
    pub complete: bool,
}

/// GitHub lists at most this many commits of a pull request, however many it holds.
const GITHUB_CAP: usize = 250;
const GITHUB_PAGE: usize = 100;

/// The commits of `change`, oldest first. Blocks on the CLI; call it off the UI thread.
pub fn list(change: &ChangeRef) -> Result<CommitRead, crate::ForgeError> {
    match change.forge {
        Forge::GitHub => github_list(change),
        Forge::GitLab => {
            let base = format!("projects/{}/merge_requests/{}/commits?", project_path(&change.repo), change.number);
            let (found, cut) = pages::<GlCommit>(change.host_name(), &base)?;
            Ok(CommitRead { commits: gitlab_commits(found), complete: !cut })
        }
    }
}

fn github_list(change: &ChangeRef) -> Result<CommitRead, crate::ForgeError> {
    let mut commits = Vec::new();
    for page in 1..=GITHUB_CAP.div_ceil(GITHUB_PAGE) {
        let path = format!("repos/{}/pulls/{}/commits?per_page={GITHUB_PAGE}&page={page}", change.repo, change.number);
        let got = github_commits(&run_on(change.host_name(), "gh", &["api", &path])?)?;
        let full = got.len() >= GITHUB_PAGE;
        commits.extend(got);
        if !full {
            break;
        }
    }
    Ok(CommitRead { complete: commits.len() < GITHUB_CAP, commits })
}

#[derive(Deserialize)]
struct GhCommit {
    sha: String,
    commit: GhInner,
    author: Option<GhLogin>,
    #[serde(default)]
    parents: Vec<GhParent>,
}

#[derive(Deserialize)]
struct GhInner {
    #[serde(default)]
    message: String,
    author: Option<GhSignature>,
    committer: Option<GhSignature>,
}

#[derive(Deserialize)]
struct GhSignature {
    name: Option<String>,
    date: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct GhLogin {
    login: String,
}

#[derive(Deserialize)]
struct GhParent {
    sha: String,
}

/// GitHub lists a pull request's commits oldest first.
fn github_commits(bytes: &[u8]) -> Result<Vec<Commit>, crate::ForgeError> {
    let found: Vec<GhCommit> = from_json(bytes)?;
    Ok(found
        .into_iter()
        .map(|c| Commit {
            sha: c.sha,
            title: first_line(&c.commit.message),
            author: c.commit.author.as_ref().and_then(|a| a.name.clone()).or(c.author.map(|a| a.login)).unwrap_or_default(),
            at: c.commit.author.as_ref().and_then(|a| a.date).or(c.commit.committer.and_then(|a| a.date)),
            merge: c.parents.len() > 1,
            parent: c.parents.into_iter().next().map(|p| p.sha),
        })
        .collect())
}

#[derive(Deserialize)]
struct GlCommit {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    author_name: String,
    committed_date: Option<DateTime<Utc>>,
    #[serde(default)]
    parent_ids: Vec<String>,
}

/// GitLab lists a merge request's commits newest first.
fn gitlab_commits(mut found: Vec<GlCommit>) -> Vec<Commit> {
    found.reverse();
    found
        .into_iter()
        .map(|c| Commit {
            sha: c.id,
            title: first_line(&c.title),
            author: c.author_name,
            at: c.committed_date,
            merge: c.parent_ids.len() > 1,
            parent: c.parent_ids.into_iter().next(),
        })
        .collect()
}

fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or_default().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped like `GET /pulls/:n/commits`: oldest first; the second has a deleted account (no `author`), a merge-style
    // message with a body, and two parents.
    #[test]
    fn github_commits_keep_order_parent_and_a_one_line_title() {
        let json = br#"[
            {"sha":"a1","commit":{"message":"Add queue\n\nLonger why.","author":{"name":"Nina K","date":"2026-11-10T03:00:00Z"},
              "committer":{"name":"web","date":"2026-11-10T04:00:00Z"}},"author":{"login":"nina.k"},"parents":[{"sha":"a0"}]},
            {"sha":"b2","commit":{"message":"Merge main","author":null,"committer":{"name":"web","date":"2026-11-11T04:00:00Z"}},
             "author":{"login":"r.amini"},"parents":[{"sha":"a1"},{"sha":"z9"}]}]"#;
        let commits = github_commits(json).expect("parses");
        assert_eq!(commits.iter().map(|c| c.sha.as_str()).collect::<Vec<_>>(), ["a1", "b2"]);
        assert_eq!((commits[0].title.as_str(), commits[0].author.as_str()), ("Add queue", "Nina K"));
        assert_eq!(commits[1].parent, Some(commits[0].sha.clone()), "the first parent is kept, the second dropped");
        assert_eq!(commits[1].author, "r.amini", "no commit author falls back to the account login");
        assert!(commits[1].at.is_some(), "no author date falls back to the committer date");
    }

    // Shaped like `GET /merge_requests/:iid/commits`: newest first.
    #[test]
    fn gitlab_commits_are_reversed_to_oldest_first() {
        let json = br#"[
            {"id":"b2","title":"Second\nignored","author_name":"R Amini","committed_date":"2026-11-11T04:00:00.000+00:00","parent_ids":["a1"]},
            {"id":"a1","title":"First","author_name":"Nina K","committed_date":"2026-11-10T04:00:00.000+00:00","parent_ids":[]}]"#;
        let commits = gitlab_commits(from_json(json).expect("parses"));
        assert_eq!(commits.iter().map(|c| c.sha.as_str()).collect::<Vec<_>>(), ["a1", "b2"]);
        assert_eq!(commits[1].parent.as_deref(), Some("a1"));
        assert_eq!(commits[0].parent, None, "a root commit has no parent");
        assert_eq!(commits[1].title, "Second");
    }
}
