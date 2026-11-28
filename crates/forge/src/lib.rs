//! Reads the forge CLIs (`gh`, `glab`). PR Craft holds no token: the CLIs keep the logins.

pub mod act;
pub mod blob;
pub mod bounded;
pub mod checkout;
pub mod commits;
pub mod error;
pub mod mine;
pub mod review;
pub mod stack;
pub mod threads;
pub mod tool;
pub mod turn;

use std::io;
use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, Utc};
use pr_craft_core::{Change, Forge};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use error::{FailKind, ForgeError};

/// Whether a forge's CLI can act for the user, and the exact command that fixes it when not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Login {
    LoggedIn,
    LoggedOut {
        fix: String,
    },
    CliMissing {
        fix: String,
    },
    /// The CLI is older than the version PR Craft is tested with; `message` says which.
    CliOld {
        message: String,
        fix: String,
    },
    /// The token lacks a scope the forge calls need.
    Scope {
        fix: String,
    },
    Failed {
        error: String,
    },
}

/// How long a call to a forge CLI may take before it is stopped, with whatever it started. A CLI that waits on a dead socket would
/// otherwise hold its caller for good.
pub(crate) const CALL_LIMIT: Duration = Duration::from_secs(120);

/// How long `<cli> auth status` may take.
const LOGIN_LIMIT: Duration = Duration::from_secs(30);

/// What a failure to run `cli` says: it did not start, or it ran out of time.
fn ran(cli: &str, error: &io::Error) -> ForgeError {
    let message =
        if error.kind() == io::ErrorKind::TimedOut { format!("{cli} did not answer: {error}") } else { format!("{cli} could not run: {error}") };
    ForgeError::spawn(error, message)
}

/// What a write that ran out of time says. The CLI was killed, but the request may have reached the forge first (the answer is what
/// was lost), so posting again could post twice.
pub const UNKNOWN_OUTCOME: &str = "The post may already have gone through: check the PR before posting again.";

/// Whether `error` is a write that ran out of time, so its outcome is not known.
pub fn outcome_unknown(error: impl AsRef<str>) -> bool {
    error.as_ref().contains(UNKNOWN_OUTCOME)
}

/// [`ran`] for a call that writes: running out of time does not mean nothing was written.
fn ran_write(cli: &str, error: &io::Error) -> ForgeError {
    if error.kind() == io::ErrorKind::TimedOut {
        ForgeError::spawn(error, format!("{cli} did not answer in time. {UNKNOWN_OUTCOME}"))
    } else {
        ran(cli, error)
    }
}

/// Asks `<cli> auth status`. A forge is signed in when any host its CLI knows is (one dead host, such as an unused
/// gitlab.com without a token, does not pause the others). Blocks for as long as the CLI takes; call it off the UI thread.
pub fn login(forge: Forge) -> Login {
    let text = match auth_text(forge) {
        Ok(text) => text,
        Err(login) => return login,
    };
    read_login(forge, &text)
}

fn read_login(forge: Forge, text: &str) -> Login {
    if forge == Forge::GitHub && missing_scopes(text) {
        Login::Scope { fix: SCOPE_FIX.into() }
    } else if parse_auth_status(text).iter().any(|h| h.signed_in) {
        Login::LoggedIn
    } else {
        signed_out(forge)
    }
}

/// One server a forge CLI lists in `auth status` (github.com, a GitHub Enterprise host, gitlab.com, a self-hosted GitLab).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeHost {
    pub name: String,
    pub signed_in: bool,
}

/// Every host the forge's CLI knows and whether it can act there. Both CLIs exit non-zero when any one host fails,
/// so the answer is read per host from what they print. Blocks on the CLI.
pub fn hosts(forge: Forge) -> Result<Vec<ForgeHost>, Login> {
    auth_text(forge).map(|text| parse_auth_status(&text))
}

/// What `<cli> auth status` prints, or why it could not be asked.
fn auth_text(forge: Forge) -> Result<String, Login> {
    let out = bounded::run(Command::new(tool::resolve(forge.cli())).args(["auth", "status"]), LOGIN_LIMIT).map_err(|e| classify(forge, Err(e)))?;
    Ok(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// What the forge calls need of a GitHub token, and the command that adds what is missing.
const SCOPE_FIX: &str = "gh auth refresh -s repo,read:org";

/// Whether gh's `auth status` says the token lacks a scope it needs.
fn missing_scopes(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("missing required") && text.contains("scope")
}

/// The login of a forge whose CLI lists no signed-in host: an old CLI is named, else the user is asked to log in.
pub fn signed_out(forge: Forge) -> Login {
    match tool::old_note(forge.cli()) {
        Some(message) => Login::CliOld { message, fix: format!("brew upgrade {}", forge.cli()) },
        None => classify(forge, Ok(false)),
    }
}

/// gh and glab print the same shape: a host line at column 0, indented lines under it, and "Logged in to" under a host
/// that works. gh repeats a host once per account; one signed-in account is enough.
fn parse_auth_status(text: &str) -> Vec<ForgeHost> {
    let mut hosts: Vec<ForgeHost> = Vec::new();
    for line in text.lines() {
        let word = line.trim();
        if !line.starts_with(char::is_whitespace) && !word.is_empty() && !word.contains(' ') && word.contains('.') {
            hosts.push(ForgeHost { name: word.to_ascii_lowercase(), signed_in: false });
        } else if let Some(host) = hosts.last_mut().filter(|_| word.contains("Logged in to")) {
            host.signed_in = true;
        }
    }
    hosts
}

pub(crate) fn signed_in_hosts(forge: Forge) -> Result<Vec<String>, ForgeError> {
    let hosts = hosts(forge).map_err(|login| {
        let kind = match login {
            Login::LoggedOut { .. } => FailKind::Auth,
            Login::CliMissing { .. } => FailKind::CliMissing,
            _ => FailKind::Other,
        };
        ForgeError::new(kind, format!("{} cannot act for you: {login:?}", forge.cli()))
    })?;
    Ok(hosts.into_iter().filter(|h| h.signed_in).map(|h| h.name).collect())
}

fn classify(forge: Forge, ran: io::Result<bool>) -> Login {
    match ran {
        Ok(true) => Login::LoggedIn,
        Ok(false) => Login::LoggedOut { fix: format!("{} auth login", forge.cli()) },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Login::CliMissing { fix: format!("brew install {}", forge.cli()) },
        Err(e) => Login::Failed { error: e.to_string() },
    }
}

/// The most pull requests one GitHub host returns per sync; a full page means the queue may hold more.
pub const PAGE: usize = 100;

/// Open changes on `host` where the user is a requested reviewer, newest first, and whether the host held more than was
/// read: GitHub one search page of [`PAGE`], GitLab page by page up to `turn::PAGES` pages. The caller walks the hosts
/// from [`hosts`], so one host failing leaves the others listed. Blocks on the CLI; call it off the UI thread.
pub fn review_requested_on(forge: Forge, host: &str) -> Result<(Vec<Change>, bool), ForgeError> {
    match forge {
        Forge::GitHub => {
            let (fields, limit) = ("number,title,repository,author,url,updatedAt,isDraft", format!("--limit={PAGE}"));
            let args = ["search", "prs", "--review-requested=@me", "--state=open", "--sort=updated", &limit, "--json", fields];
            let found = parse_github(&run_on(host, "gh", &args)?, host)?;
            let truncated = found.len() >= PAGE;
            Ok((found, truncated))
        }
        Forge::GitLab => {
            let me: GitLabUser = from_json(&run_on(host, "glab", &["api", "user"])?)?;
            let base = format!("merge_requests?state=opened&scope=all&order_by=updated_at&reviewer_username={}&", me.username);
            let (mrs, cut) = turn::pages::<GitLabMr>(host, &base)?;
            Ok((mrs.into_iter().map(|mr| gitlab_change(mr, host)).collect(), cut))
        }
    }
}

/// Both CLIs pick their server from the environment before the git remote of the working directory (`GH_HOST` for gh,
/// `GITLAB_HOST` for glab), so setting it always sends the call to `host`. An empty `host` leaves the CLI's own choice.
pub(crate) fn command(host: &str, cli: &str) -> Command {
    let mut command = Command::new(tool::resolve(cli));
    if !host.is_empty() {
        match cli {
            "glab" => command.env("GITLAB_HOST", host),
            "gh" => command.env("GH_HOST", host),
            _ => &mut command,
        };
    }
    command
}

/// Runs `cli` against `host` within the call deadline. Every forge read goes through here, so none can hang the UI.
pub(crate) fn run_on(host: &str, cli: &str, args: &[&str]) -> Result<Vec<u8>, ForgeError> {
    run_limited(host, cli, args, CALL_LIMIT)
}

fn run_limited(host: &str, cli: &str, args: &[&str], limit: Duration) -> Result<Vec<u8>, ForgeError> {
    let out = bounded::run(command(host, cli).args(args), limit).map_err(|e| ran(cli, &e))?;
    finished(cli, args, out)
}

/// A call that writes to the forge, without input (an approval, a GraphQL mutation). A timeout says the post may have gone through.
pub(crate) fn run_write_on(host: &str, cli: &str, args: &[&str]) -> Result<Vec<u8>, ForgeError> {
    write_limited(host, cli, args, None, CALL_LIMIT)
}

/// Like [`run_write_on`], with `input` piped to the CLI's stdin. Every user of this posts something.
pub(crate) fn run_with_input_on(host: &str, cli: &str, args: &[&str], input: &[u8]) -> Result<Vec<u8>, ForgeError> {
    write_limited(host, cli, args, Some(input), CALL_LIMIT)
}

fn write_limited(host: &str, cli: &str, args: &[&str], input: Option<&[u8]>, limit: Duration) -> Result<Vec<u8>, ForgeError> {
    let mut command = command(host, cli);
    command.args(args);
    let out = match input {
        Some(input) => bounded::run_with_input(&mut command, input, limit),
        None => bounded::run(&mut command, limit),
    }
    .map_err(|e| ran_write(cli, &e))?;
    finished(cli, args, out)
}

fn finished(cli: &str, args: &[&str], out: std::process::Output) -> Result<Vec<u8>, ForgeError> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let message = format!("`{cli} {}` failed: {}", args.join(" "), stderr.trim());
        let error = ForgeError::exited(message, &stderr);
        // An unexplained failure of a CLI below the tested minimum is most likely the CLI's age.
        match tool::old_note(cli).filter(|_| error.kind == FailKind::Other) {
            Some(note) => Err(ForgeError::new(FailKind::CliOld, format!("{}. {note}: run `brew upgrade {cli}`.", error.message))),
            None => Err(error),
        }
    }
}

/// What one GitHub GraphQL call cost and what is left of the hour's points (`rateLimit` in the answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub cost: u32,
    pub remaining: u32,
    pub reset_at: DateTime<Utc>,
}

/// The `rateLimit` of a `gh api graphql` answer; `None` when it carries none.
pub fn rate_limit(bytes: &[u8]) -> Option<RateLimit> {
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "rateLimit")]
        rate_limit: Option<RateLimit>,
    }
    #[derive(Deserialize)]
    struct Answer {
        data: Option<Data>,
    }
    serde_json::from_slice::<Answer>(bytes).ok()?.data?.rate_limit
}

pub(crate) fn from_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ForgeError> {
    serde_json::from_slice(bytes).map_err(|e| ForgeError::from(format!("unexpected CLI output: {e}")))
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn a_cli_that_does_not_answer_is_stopped_and_the_error_says_it_did_not_answer() {
        let error = run_limited("", "sleep", &["30"], Duration::from_millis(200)).expect_err("stopped");
        assert_eq!(error.kind, FailKind::Timeout);
        assert!(error.message.starts_with("sleep did not answer"), "{error}");
        assert_eq!(run_limited("", "echo", &["hi"], Duration::from_secs(10)).expect("ran"), b"hi\n");
        assert!(!outcome_unknown(&error), "a read that ran out of time wrote nothing, so there is nothing to check");
    }

    #[test]
    fn a_write_that_does_not_answer_says_it_may_already_have_gone_through() {
        for input in [None, Some(&b"{}"[..])] {
            let error = write_limited("", "sleep", &["30"], input, Duration::from_millis(200)).expect_err("stopped");
            assert_eq!(error.message, "sleep did not answer in time. The post may already have gone through: check the PR before posting again.");
            assert!(outcome_unknown(&error));
        }
        let refused = write_limited("", "false", &[], None, Duration::from_secs(10)).expect_err("failed at once");
        assert!(!outcome_unknown(&refused), "a CLI that refused or failed cleanly wrote nothing: {refused}");
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubPr {
    number: u64,
    title: String,
    repository: GitHubRepo,
    author: GitHubUser,
    url: String,
    updated_at: DateTime<Utc>,
    is_draft: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubRepo {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct GitHubUser {
    login: String,
}

fn parse_github(bytes: &[u8], host: &str) -> Result<Vec<Change>, ForgeError> {
    let prs: Vec<GitHubPr> = from_json(bytes)?;
    Ok(prs
        .into_iter()
        .map(|pr| Change {
            forge: Forge::GitHub,
            repo: pr.repository.name_with_owner,
            number: pr.number,
            title: pr.title,
            author: pr.author.login,
            url: pr.url,
            host: Forge::GitHub.host_field(host),
            updated_at: pr.updated_at,
            stack: None,
            draft: pr.is_draft,
        })
        .collect())
}

#[derive(Deserialize)]
pub(crate) struct GitLabMr {
    iid: u64,
    title: String,
    web_url: String,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    draft: bool,
    /// What older GitLab servers send instead of `draft`.
    #[serde(default)]
    work_in_progress: bool,
    author: GitLabUser,
    references: GitLabReferences,
}

#[derive(Deserialize)]
struct GitLabUser {
    username: String,
}

#[derive(Deserialize)]
struct GitLabReferences {
    /// `group/subgroup/project!88`.
    full: String,
}

#[cfg(test)]
fn parse_gitlab(bytes: &[u8], host: &str) -> Result<Vec<Change>, ForgeError> {
    let mrs: Vec<GitLabMr> = from_json(bytes)?;
    Ok(mrs.into_iter().map(|mr| gitlab_change(mr, host)).collect())
}

pub(crate) fn gitlab_change(mr: GitLabMr, host: &str) -> Change {
    Change {
        forge: Forge::GitLab,
        repo: mr.references.full.rsplit_once('!').map_or(mr.references.full.as_str(), |(path, _)| path).to_string(),
        number: mr.iid,
        title: mr.title,
        author: mr.author.username,
        url: mr.web_url,
        host: Forge::GitLab.host_field(host),
        updated_at: mr.updated_at,
        stack: None,
        draft: mr.draft || mr.work_in_progress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_limit_is_read_from_a_graphql_answer() {
        let answer = br#"{"data":{"search":{"nodes":[]},"rateLimit":{"cost":57,"remaining":4943,"resetAt":"2026-11-28T10:00:00Z"}}}"#;
        let limit = rate_limit(answer).expect("a rate limit");
        assert_eq!((limit.cost, limit.remaining), (57, 4943));
        assert_eq!(limit.reset_at.to_rfc3339(), "2026-11-28T10:00:00+00:00");
        assert_eq!(rate_limit(br#"{"data":{"search":{}}}"#), None);
        assert_eq!(rate_limit(b"[]"), None);
    }

    #[test]
    fn each_failure_names_a_fix_for_that_forges_cli() {
        for forge in Forge::ALL {
            let Login::LoggedOut { fix } = classify(forge, Ok(false)) else { panic!("a failed status is a logged-out CLI") };
            assert!(fix.starts_with(forge.cli()), "{fix}");
            let missing = classify(forge, Err(io::ErrorKind::NotFound.into()));
            let Login::CliMissing { fix } = missing else { panic!("a CLI not on PATH is missing, got {missing:?}") };
            assert!(fix.ends_with(forge.cli()), "{fix}");
        }
        assert!(matches!(classify(Forge::GitHub, Err(io::ErrorKind::PermissionDenied.into())), Login::Failed { .. }));
    }

    #[test]
    fn a_token_missing_scopes_is_a_scope_login_with_the_command_that_adds_them() {
        let text = "github.com\n  X Failed to log in to github.com account ada (keyring)\n  - Active account: true\n  - The token in keyring is missing required scopes: 'read:org'\n";
        assert_eq!(read_login(Forge::GitHub, text), Login::Scope { fix: "gh auth refresh -s repo,read:org".into() });
        let fine = "github.com\n  ✓ Logged in to github.com account ada (keyring)\n  - Token scopes: 'read:org', 'repo'\n";
        assert_eq!(read_login(Forge::GitHub, fine), Login::LoggedIn);
    }

    #[test]
    fn github_search_output_becomes_changes() {
        let json = br#"[{"author":{"login":"nina.k","is_bot":false},"isDraft":true,"number":482,
            "repository":{"name":"api","nameWithOwner":"acme/api"},"title":"Retry webhooks",
            "updatedAt":"2026-11-17T03:34:10Z","url":"https://github.com/acme/api/pull/482"}]"#;
        let changes = parse_github(json, "github.com").expect("gh output parses");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].reference(), "acme/api #482");
        assert_eq!(changes[0].author, "nina.k");
        assert!(changes[0].draft);
        assert_eq!(changes[0].host, None, "github.com stores no host, so old keys match");
        assert_eq!(parse_github(json, "ghe.acme.dev").expect("parses")[0].change_ref().host_name(), "ghe.acme.dev");
    }

    // Fixtures: invented names, shaped like `gh search prs --json` and GitLab's `GET /merge_requests` output.
    #[test]
    fn gitlab_project_path_keeps_subgroups_and_drops_the_number() {
        let json = br#"[{"iid":88,"title":"Pin Terraform AWS provider","web_url":"https://gitlab.acme.dev/infra/aws/modules/-/merge_requests/88",
            "updated_at":"2026-11-17T03:34:10.123Z","author":{"username":"r.amini"},
            "references":{"short":"!88","full":"infra/aws/modules!88"}},
            {"iid":89,"title":"Old server draft","web_url":"https://gitlab.acme.dev/infra/aws/modules/-/merge_requests/89",
            "updated_at":"2026-11-17T03:34:10.123Z","author":{"username":"r.amini"},"work_in_progress":true,
            "references":{"short":"!89","full":"infra/aws/modules!89"}}]"#;
        let changes = parse_gitlab(json, "gitlab.acme.dev").expect("glab output parses");
        assert_eq!(changes[0].reference(), "infra/aws/modules !88");
        assert_eq!(changes[0].change_ref().host_name(), "gitlab.acme.dev", "every later call goes back to the host it came from");
        assert!(!changes[0].draft, "a server without `draft` means not a draft");
        assert!(changes[1].draft, "`work_in_progress` from an older server marks a draft");
        assert_eq!(parse_gitlab(json, "GitLab.com").expect("parses")[0].host, None, "the default host stores no host, so old keys match");
    }

    // Shaped like glab 1.99 `auth status` with one host lacking a token; names invented.
    #[test]
    fn glab_status_is_read_per_host_and_a_dead_default_host_does_not_hide_a_working_one() {
        let text = "gitlab.com\n  x gitlab.com: API call failed: GET https://gitlab.com/api/v4/user: 401 {message: 401 Unauthorized}\n  \
                    ! No token found (checked config file, keyring, and environment variables).\ngitlab.example.go.th\n  \
                    ✓ Logged in to gitlab.example.go.th as someone (/Users/x/config.yml)\n  ✓ Token found: ****\n\n   ERROR\n  \
                    X could not authenticate to one or more of the configured GitLab instances..\n";
        let hosts = parse_auth_status(text);
        assert_eq!(
            hosts,
            [ForgeHost { name: "gitlab.com".into(), signed_in: false }, ForgeHost { name: "gitlab.example.go.th".into(), signed_in: true }]
        );
    }

    // Shaped like gh 2.89 `auth status`: two accounts on github.com, an Enterprise host that failed; names invented.
    #[test]
    fn gh_status_counts_a_host_once_and_reads_an_enterprise_host() {
        let text = "github.com\n  ✓ Logged in to github.com account ada (keyring)\n  - Active account: true\n\n  \
                    ✓ Logged in to github.com account bot (keyring)\n  - Active account: false\n\nghe.acme.dev\n  \
                    X Failed to log in to ghe.acme.dev using token (GH_ENTERPRISE_TOKEN)\n";
        assert_eq!(
            parse_auth_status(text),
            [ForgeHost { name: "github.com".into(), signed_in: true }, ForgeHost { name: "ghe.acme.dev".into(), signed_in: false }]
        );
    }

    #[test]
    fn unexpected_output_is_an_error_not_an_empty_queue() {
        assert!(parse_github(b"{\"message\":\"rate limited\"}", "github.com").is_err());
        assert!(parse_gitlab(b"", "gitlab.com").is_err());
    }
}
