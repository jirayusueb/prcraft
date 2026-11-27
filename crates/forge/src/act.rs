//! Changes to one of the user's own changes on the forge: ready for review, reviewers, labels, merging and re-running a job, and
//! the reads the pickers need. A write is built as data first ([`Write`]) so what it would send is tested without sending it, and
//! it is sent through the write path of this crate: a call that runs out of time says it may already have been applied.

use std::time::Duration;

use pr_craft_core::mine::MergeMethod;
use pr_craft_core::{ChangeRef, Forge};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::review::project_path;
use crate::{CALL_LIMIT, from_json, mine, run_on, write_limited};

/// One CLI call that changes a forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub cli: &'static str,
    /// The server the call goes to.
    pub host: String,
    pub args: Vec<String>,
    /// Piped to the CLI's stdin.
    pub input: Option<String>,
}

impl Write {
    fn new(change: &ChangeRef, args: &[&str], input: Option<Value>) -> Write {
        Write {
            cli: change.forge.cli(),
            host: change.host_name().to_string(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
            input: input.map(|v| v.to_string()),
        }
    }

    /// Sends it. Blocks on the CLI. A call that runs out of time says the change may already have been made
    /// ([`crate::outcome_unknown`]).
    pub fn send(&self) -> Result<(), String> {
        self.send_within(CALL_LIMIT)
    }

    fn send_within(&self, limit: Duration) -> Result<(), String> {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        write_limited(&self.host, self.cli, &args, self.input.as_deref().map(str::as_bytes), limit).map(drop)
    }
}

fn json_input(change: &ChangeRef, path: &str, method: &str, body: Value) -> Write {
    // gh sends JSON by itself and takes no header for a body read from stdin; glab needs the type said.
    let args: Vec<&str> = match change.forge {
        Forge::GitHub => vec!["api", path, "--method", method, "--input", "-"],
        Forge::GitLab => vec!["api", path, "--method", method, "-H", "Content-Type: application/json", "--input", "-"],
    };
    Write::new(change, &args, Some(body))
}

fn mr_path(change: &ChangeRef, tail: &str) -> String {
    format!("projects/{}/merge_requests/{}{tail}", project_path(&change.repo), change.number)
}

/// Marks the draft ready for review.
pub fn ready(change: &ChangeRef) -> Write {
    let number = change.number.to_string();
    match change.forge {
        Forge::GitHub => Write::new(change, &["pr", "ready", &number, "-R", &change.repo], None),
        // The API's update call has no draft attribute; the CLI's `--ready` takes the Draft: prefix off.
        Forge::GitLab => Write::new(change, &["mr", "update", &number, "-R", &change.repo, "--ready"], None),
    }
}

/// GitHub: the calls that ask `add` and take `remove` off the review requests. Adding comes first, so a failure in between never
/// leaves the change with fewer reviewers than it had.
pub fn github_reviewers(change: &ChangeRef, add: &[String], remove: &[String]) -> Vec<Write> {
    let path = format!("repos/{}/pulls/{}/requested_reviewers", change.repo, change.number);
    [("POST", add), ("DELETE", remove)]
        .into_iter()
        .filter(|(_, who)| !who.is_empty())
        .map(|(method, who)| json_input(change, &path, method, json!({ "reviewers": who })))
        .collect()
}

/// GitLab: sets the reviewers to the users with `ids`.
pub fn gitlab_reviewers(change: &ChangeRef, ids: &[u64]) -> Write {
    json_input(change, &mr_path(change, ""), "PUT", json!({ "reviewer_ids": ids }))
}

#[derive(Deserialize)]
struct GlId {
    id: u64,
}

/// A login is letters, digits and `-_.`; anything else is refused before it reaches a URL.
fn checked_login(login: &str) -> Result<&str, String> {
    if login.is_empty() || !login.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(format!("{login:?} is not a login"));
    }
    Ok(login)
}

/// Makes `want` the change's reviewers. `current` is who is asked now (a reviewer who already reviewed is not asked, and stays).
/// A set that is already right sends nothing. Blocks on the CLI.
pub fn set_reviewers(change: &ChangeRef, current: &[String], want: &[String]) -> Result<usize, String> {
    for login in want {
        checked_login(login)?;
    }
    let (add, remove): (Vec<String>, Vec<String>) = (
        want.iter().filter(|w| !current.iter().any(|c| c.eq_ignore_ascii_case(w))).cloned().collect(),
        current.iter().filter(|c| !want.iter().any(|w| w.eq_ignore_ascii_case(c))).cloned().collect(),
    );
    if add.is_empty() && remove.is_empty() {
        return Ok(0);
    }
    let writes = match change.forge {
        Forge::GitHub => github_reviewers(change, &add, &remove),
        Forge::GitLab => {
            let mut ids = Vec::new();
            for login in want {
                let found: Vec<GlId> = from_json(&run_on(change.host_name(), "glab", &["api", &format!("users?username={login}")])?)?;
                ids.push(found.first().map(|u| u.id).ok_or_else(|| format!("GitLab has no user {login}"))?);
            }
            vec![gitlab_reviewers(change, &ids)]
        }
    };
    for write in &writes {
        write.send()?;
    }
    Ok(add.len() + remove.len())
}

/// The calls that add `add` to the change's labels and take `remove` off, relative to what they are now, so a label someone added
/// since the reader looked is left alone.
pub fn change_labels(change: &ChangeRef, add: &[String], remove: &[String]) -> Result<Vec<Write>, String> {
    if add.iter().chain(remove).any(|l| l.trim().is_empty()) {
        return Err("A label needs a name".into());
    }
    // `.` and `..` are path segments, not names: `DELETE …/labels/..` would reach the whole collection.
    if add.iter().chain(remove).any(|l| !l.is_empty() && l.chars().all(|c| c == '.')) {
        return Err("A label named only dots cannot be changed here: it would be read as a path, not a name".into());
    }
    Ok(match change.forge {
        Forge::GitHub => {
            let base = format!("repos/{}/issues/{}/labels", change.repo, change.number);
            let mut writes = Vec::new();
            if !add.is_empty() {
                writes.push(json_input(change, &base, "POST", json!({ "labels": add })));
            }
            for name in remove {
                writes.push(Write::new(change, &["api", &format!("{base}/{}", crate::blob::encode(name)), "--method", "DELETE"], None));
            }
            writes
        }
        Forge::GitLab => {
            if add.iter().chain(remove).any(|l| l.contains(',')) {
                return Err("GitLab takes labels as a comma list, so a label with a comma in its name cannot be set here".into());
            }
            if add.is_empty() && remove.is_empty() {
                return Ok(Vec::new());
            }
            let body = json!({ "add_labels": add.join(","), "remove_labels": remove.join(",") });
            vec![json_input(change, &mr_path(change, ""), "PUT", body)]
        }
    })
}

/// Replaces the change's labels with `labels`.
pub fn set_labels(change: &ChangeRef, labels: &[String]) -> Result<Write, String> {
    if labels.iter().any(|l| l.trim().is_empty()) {
        return Err("A label needs a name".into());
    }
    Ok(match change.forge {
        Forge::GitHub => json_input(change, &format!("repos/{}/issues/{}/labels", change.repo, change.number), "PUT", json!({ "labels": labels })),
        Forge::GitLab => {
            if labels.iter().any(|l| l.contains(',')) {
                return Err("GitLab takes labels as a comma list, so a label with a comma in its name cannot be set here".into());
            }
            json_input(change, &mr_path(change, ""), "PUT", json!({ "labels": labels.join(",") }))
        }
    })
}

/// What a merge asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeCall {
    pub method: MergeMethod,
    /// The commit message: its first line is the subject. Empty keeps the forge's own.
    pub message: String,
    pub delete_branch: bool,
    /// The head commit the user saw; the forge refuses when the change has moved.
    pub head_sha: String,
    /// Merge when the checks pass instead of now.
    pub when_ready: bool,
}

/// The merge as one call. GitLab merges with the method the project sets, so only a squash can be asked for; a rebase is refused.
pub fn merge(change: &ChangeRef, call: &MergeCall) -> Result<Write, String> {
    if call.head_sha.trim().is_empty() {
        return Err("A merge must name the commit the reader saw, and none was given".into());
    }
    let message = call.message.trim();
    match change.forge {
        Forge::GitHub => {
            let number = change.number.to_string();
            let flag = match call.method {
                MergeMethod::Squash => "--squash",
                MergeMethod::Merge => "--merge",
                MergeMethod::Rebase => "--rebase",
            };
            let mut args = vec!["pr", "merge", number.as_str(), "-R", change.repo.as_str(), flag];
            let (subject, body) = message.split_once('\n').map_or((message, ""), |(s, b)| (s.trim(), b.trim()));
            if !message.is_empty() && call.method != MergeMethod::Rebase {
                args.extend(["--subject", subject]);
                if !body.is_empty() {
                    args.extend(["--body", body]);
                }
            }
            if call.delete_branch {
                args.push("--delete-branch");
            }
            if call.when_ready {
                args.push("--auto");
            }
            if !call.head_sha.is_empty() {
                args.extend(["--match-head-commit", call.head_sha.as_str()]);
            }
            Ok(Write::new(change, &args, None))
        }
        Forge::GitLab => {
            if call.method == MergeMethod::Rebase {
                return Err("GitLab merges with the method the project sets (Settings, Merge requests), so only a squash or a merge commit can be asked for here".into());
            }
            let mut body = json!({
                "squash": call.method == MergeMethod::Squash,
                "should_remove_source_branch": call.delete_branch,
                "merge_when_pipeline_succeeds": call.when_ready,
            });
            if !call.head_sha.is_empty() {
                body["sha"] = json!(call.head_sha);
            }
            if !message.is_empty() {
                body[if call.method == MergeMethod::Squash { "squash_commit_message" } else { "merge_commit_message" }] = json!(message);
            }
            Ok(json_input(change, &mr_path(change, "/merge"), "PUT", body))
        }
    }
}

/// Turns Merge when ready off.
pub fn cancel_auto(change: &ChangeRef) -> Write {
    match change.forge {
        Forge::GitHub => Write::new(change, &["pr", "merge", &change.number.to_string(), "-R", &change.repo, "--disable-auto"], None),
        Forge::GitLab => Write::new(change, &["api", &mr_path(change, "/cancel_merge_when_pipeline_succeeds"), "--method", "POST"], None),
    }
}

/// Re-runs one job. `check` is an id from `checks.list`.
pub fn rerun(change: &ChangeRef, check: &str) -> Result<Write, String> {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    match change.forge {
        Forge::GitHub => {
            let (run, job) = check
                .split_once('/')
                .filter(|(r, j)| digits(r) && digits(j))
                .ok_or("This check is not a GitHub Actions job, so it cannot be re-run here")?;
            Ok(Write::new(change, &["run", "rerun", run, "-R", &change.repo, "--job", job], None))
        }
        Forge::GitLab => {
            if !digits(check) {
                return Err(format!("{check:?} is not a GitLab job id"));
            }
            Ok(Write::new(change, &["api", &format!("projects/{}/jobs/{check}/retry", project_path(&change.repo)), "--method", "POST"], None))
        }
    }
}

/// A person a change can ask to review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Person {
    pub login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What the forge says they can do in the repository (`write`, `Maintainer`).
    pub role: String,
}

/// A label of the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Label {
    pub name: String,
    /// Six hex digits, when the forge says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub description: String,
}

/// A bounded list: one page of [`crate::PAGE`], and whether the repository holds more than that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

#[derive(Deserialize)]
struct GhPerson {
    login: String,
    #[serde(default)]
    role_name: String,
}

#[derive(Deserialize)]
struct GlMember {
    username: String,
    name: Option<String>,
    #[serde(default)]
    access_level: u32,
    #[serde(default)]
    state: String,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
    color: Option<String>,
    description: Option<String>,
}

fn gitlab_role(level: u32) -> &'static str {
    match level {
        0..=10 => "Guest",
        11..=20 => "Reporter",
        21..=30 => "Developer",
        31..=40 => "Maintainer",
        _ => "Owner",
    }
}

fn parse_people(forge: Forge, bytes: &[u8]) -> Result<Vec<Person>, String> {
    Ok(match forge {
        Forge::GitHub => from_json::<Vec<GhPerson>>(bytes)?
            .into_iter()
            .map(|p| Person { login: p.login, name: None, role: format!("{} access", p.role_name) })
            .collect(),
        Forge::GitLab => from_json::<Vec<GlMember>>(bytes)?
            .into_iter()
            .filter(|m| m.state.is_empty() || m.state == "active")
            .map(|m| Person { login: m.username, name: m.name, role: gitlab_role(m.access_level).into() })
            .collect(),
    })
}

fn parse_labels(bytes: &[u8]) -> Result<Vec<Label>, String> {
    Ok(from_json::<Vec<RawLabel>>(bytes)?
        .into_iter()
        .map(|l| Label {
            name: l.name,
            color: l.color.map(|c| c.trim_start_matches('#').to_string()).filter(|c| c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit())),
            description: l.description.unwrap_or_default(),
        })
        .collect())
}

fn one_page(host: &str, cli: &str, path: &str) -> Result<(Vec<u8>, bool), String> {
    let bytes = run_on(host, cli, &["api", &format!("{path}per_page=100")])?;
    let full = from_json::<Vec<serde::de::IgnoredAny>>(&bytes)?.len() >= 100;
    Ok((bytes, full))
}

/// Who the change can ask to review: the repository's collaborators (GitHub, which refuses a login without push access) or the
/// project's members (GitLab). One page. Blocks on the CLI.
pub fn collaborators(change: &ChangeRef) -> Result<Page<Person>, String> {
    let path = match change.forge {
        Forge::GitHub => format!("repos/{}/collaborators?", change.repo),
        Forge::GitLab => format!("projects/{}/members/all?", project_path(&change.repo)),
    };
    let (bytes, truncated) = one_page(change.host_name(), change.forge.cli(), &path)?;
    Ok(Page { items: parse_people(change.forge, &bytes)?, truncated })
}

/// The repository's labels. One page. Blocks on the CLI.
pub fn labels_of(change: &ChangeRef) -> Result<Page<Label>, String> {
    let path = match change.forge {
        Forge::GitHub => format!("repos/{}/labels?", change.repo),
        Forge::GitLab => format!("projects/{}/labels?", project_path(&change.repo)),
    };
    let (bytes, truncated) = one_page(change.host_name(), change.forge.cli(), &path)?;
    Ok(Page { items: parse_labels(&bytes)?, truncated })
}

/// Who is asked to review `change` now, read fresh.
pub fn requested_now(change: &ChangeRef) -> Result<Vec<String>, String> {
    Ok(mine::detail_on(change)?.mine.requested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome_unknown;

    fn gh() -> ChangeRef {
        ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 490, host: Some("ghe.acme.dev".into()) }
    }

    fn gl() -> ChangeRef {
        ChangeRef { forge: Forge::GitLab, repo: "infra/aws/modules".into(), number: 88, host: Some("gitlab.acme.dev".into()) }
    }

    fn call(method: MergeMethod) -> MergeCall {
        MergeCall {
            method,
            message: "fix(retry): give each worker its own source (#490)\n\nWorkers shared one.".into(),
            delete_branch: true,
            head_sha: "7c2e1f0".into(),
            when_ready: false,
        }
    }

    #[test]
    fn a_github_squash_names_its_method_message_branch_and_the_commit_the_user_saw() {
        let w = merge(&gh(), &call(MergeMethod::Squash)).expect("builds");
        assert_eq!((w.cli, w.host.as_str()), ("gh", "ghe.acme.dev"), "the call goes to the host the change came from");
        assert_eq!(
            w.args,
            [
                "pr",
                "merge",
                "490",
                "-R",
                "acme/api",
                "--squash",
                "--subject",
                "fix(retry): give each worker its own source (#490)",
                "--body",
                "Workers shared one.",
                "--delete-branch",
                "--match-head-commit",
                "7c2e1f0"
            ]
        );
        let auto = merge(&gh(), &MergeCall { when_ready: true, ..call(MergeMethod::Merge) }).expect("builds");
        assert!(auto.args.contains(&"--auto".to_string()) && auto.args.contains(&"--merge".to_string()));
        let rebase = merge(&gh(), &call(MergeMethod::Rebase)).expect("builds");
        assert!(!rebase.args.iter().any(|a| a == "--subject"), "a rebase writes no commit message, so none is sent");
    }

    #[test]
    fn a_gitlab_merge_squashes_through_the_api_and_refuses_a_rebase() {
        let w = merge(&gl(), &call(MergeMethod::Squash)).expect("builds");
        assert_eq!(w.args[..4], ["api", "projects/infra%2Faws%2Fmodules/merge_requests/88/merge", "--method", "PUT"]);
        let body: Value = serde_json::from_str(w.input.as_deref().expect("a body")).expect("json");
        assert_eq!(
            (body["squash"].clone(), body["should_remove_source_branch"].clone(), body["sha"].clone()),
            (json!(true), json!(true), json!("7c2e1f0"))
        );
        assert!(body["squash_commit_message"].as_str().is_some_and(|m| m.starts_with("fix(retry)")));
        let plain = merge(&gl(), &call(MergeMethod::Merge)).expect("builds");
        let body: Value = serde_json::from_str(plain.input.as_deref().expect("a body")).expect("json");
        assert_eq!(body["squash"], json!(false));
        assert!(body.get("merge_commit_message").is_some());
        assert!(merge(&gl(), &call(MergeMethod::Rebase)).expect_err("refused").contains("project sets"));
    }

    #[test]
    fn reviewers_are_asked_before_they_are_taken_off_and_an_unchanged_set_sends_nothing() {
        let writes = github_reviewers(&gh(), &["j.park".into()], &["nina.k".into()]);
        assert_eq!(
            writes.iter().map(|w| w.args[3].as_str()).collect::<Vec<_>>(),
            ["POST", "DELETE"],
            "adding first never leaves the change short of reviewers"
        );
        assert_eq!(writes[0].args[1], "repos/acme/api/pulls/490/requested_reviewers");
        assert!(github_reviewers(&gh(), &[], &[]).is_empty());
        assert_eq!(set_reviewers(&gh(), &["nina.k".into()], &["Nina.K".into()]), Ok(0), "the same person in another case is not a change");
        assert!(set_reviewers(&gh(), &[], &["a b".into()]).is_err(), "a login with a space is refused before any call");
    }

    #[test]
    fn a_label_change_sends_only_what_the_reader_changed() {
        assert!(change_labels(&gh(), &[], &["..".into()]).is_err() && change_labels(&gh(), &[], &[".".into()]).is_err(), "never a raw dot segment");
        assert!(change_labels(&gl(), &["...".into()], &[]).is_err());
        let writes = change_labels(&gh(), &["bug".into()], &["size/XS".into()]).expect("builds");
        assert_eq!(writes.len(), 2);
        assert_eq!((writes[0].args[3].as_str(), writes[0].input.as_deref()), ("POST", Some(r#"{"labels":["bug"]}"#)));
        assert_eq!(writes[1].args[1], "repos/acme/api/issues/490/labels/size%2FXS", "the name is one encoded segment");
        assert!(change_labels(&gh(), &[], &[]).expect("builds").is_empty(), "no change sends nothing, and never the whole set");
        let gitlab = change_labels(&gl(), &["a".into()], &["b".into()]).expect("builds");
        assert_eq!(gitlab[0].input.as_deref(), Some(r#"{"add_labels":"a","remove_labels":"b"}"#));
    }

    #[test]
    fn labels_replace_the_set_and_a_gitlab_comma_is_refused() {
        let w = set_labels(&gh(), &["bug".into(), "flaky-test".into()]).expect("builds");
        assert_eq!((w.args[3].as_str(), w.input.as_deref()), ("PUT", Some(r#"{"labels":["bug","flaky-test"]}"#)));
        assert_eq!(set_labels(&gl(), &["a".into(), "b".into()]).expect("builds").input.as_deref(), Some(r#"{"labels":"a,b"}"#));
        assert!(set_labels(&gl(), &["a,b".into()]).is_err());
        assert!(set_labels(&gh(), &["  ".into()]).is_err());
    }

    #[test]
    fn a_rerun_takes_only_ids_the_forge_gave() {
        assert_eq!(
            rerun(&gh(), "39587948030/119181432015").expect("builds").args,
            ["run", "rerun", "39587948030", "-R", "acme/api", "--job", "119181432015"]
        );
        assert!(rerun(&gh(), "ci/legacy").is_err() && rerun(&gh(), "1/--help").is_err());
        assert!(rerun(&gl(), "7/retry").is_err());
        assert!(rerun(&gl(), "7").expect("builds").args[1].ends_with("/jobs/7/retry"));
    }

    #[test]
    fn auto_off_is_one_call() {
        assert!(cancel_auto(&gh()).args.contains(&"--disable-auto".to_string()));
    }

    #[test]
    fn a_merge_without_the_commit_the_reader_saw_is_refused() {
        let call = MergeCall { head_sha: String::new(), ..call(MergeMethod::Squash) };
        assert!(merge(&gh(), &call).expect_err("refused").contains("commit"));
        assert!(merge(&gl(), &call).is_err());
    }

    #[test]
    fn a_write_that_runs_out_of_time_says_it_may_already_be_applied() {
        let slow = Write { cli: "sleep", host: String::new(), args: vec!["30".into()], input: None };
        let error = slow.send_within(Duration::from_millis(200)).expect_err("stopped");
        assert!(outcome_unknown(&error), "the reader is told to check the forge before sending it again: {error}");
        let refused = Write { cli: "false", host: String::new(), args: Vec::new(), input: None };
        assert!(!outcome_unknown(&refused.send_within(Duration::from_secs(10)).expect_err("failed at once")), "a clean refusal wrote nothing");
    }

    #[test]
    fn the_pickers_read_what_each_forge_sends_and_drop_what_is_not_usable() {
        let people = parse_people(Forge::GitHub, br#"[{"login":"j.park","role_name":"write","id":1}]"#).expect("parses");
        assert_eq!((people[0].login.as_str(), people[0].role.as_str()), ("j.park", "write access"));
        let members = parse_people(
            Forge::GitLab,
            br#"[{"username":"a","name":"Ada","access_level":40,"state":"active"},{"username":"b","access_level":30,"state":"blocked"}]"#,
        )
        .expect("parses");
        assert_eq!(members.len(), 1, "a blocked member cannot review");
        assert_eq!(members[0].role, "Maintainer");
        let labels = parse_labels(br##"[{"name":"bug","color":"#D73A4A","description":null},{"name":"odd","color":"red"}]"##).expect("parses");
        assert_eq!(
            (labels[0].color.as_deref(), labels[1].color.as_deref()),
            (Some("D73A4A"), None),
            "a colour that is not six hex digits is dropped"
        );
        assert!(parse_labels(b"{}").is_err());
    }
}
