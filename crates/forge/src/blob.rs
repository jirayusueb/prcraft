//! Reads one file as it was at one commit, through `gh` and `glab`.

use std::fmt::Write as _;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::Stdio;

use pr_craft_core::{ChangeRef, Forge};

/// The most bytes of one file a view reads.
pub const MAX_BLOB: usize = 20 << 20;

/// The words every caller shows when a file passes [`MAX_BLOB`].
pub const TOO_LARGE: &str = "too large to show here";

/// The bytes of `path` at commit `sha`, or `None` when the commit has no such file (the base side of an added
/// file, the head side of a deleted one). A file over [`MAX_BLOB`] is an error saying [`TOO_LARGE`]. Blocks on the CLI.
pub fn blob(change: &ChangeRef, sha: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    let (cli, args) = match change.forge {
        Forge::GitHub => ("gh", github_args(&change.repo, sha, path)),
        Forge::GitLab => ("glab", gitlab_args(&change.repo, sha, path)),
    };
    fetch_on(change.host_name(), cli, &args, MAX_BLOB)
}

/// `gh api` asks for the raw bytes, so no JSON or base64 wraps them.
fn github_args(repo: &str, sha: &str, path: &str) -> Vec<String> {
    let path: Vec<String> = path.split('/').map(encode).collect();
    vec![
        "api".into(),
        "-H".into(),
        "Accept: application/vnd.github.raw".into(),
        format!("repos/{repo}/contents/{}?ref={}", path.join("/"), encode(sha)),
    ]
}

/// GitLab takes the whole file path, slashes included, as one segment.
fn gitlab_args(repo: &str, sha: &str, path: &str) -> Vec<String> {
    vec!["api".into(), format!("projects/{}/repository/files/{}/raw?ref={}", encode(repo), encode(path), encode(sha))]
}

/// Percent-encodes every byte that is not unreserved (RFC 3986).
pub(crate) fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Whether a CLI failure says the file is not at that commit. GitHub also answers 404 for an unknown commit, which is an error.
fn not_found(stderr: &str) -> bool {
    (stderr.contains("HTTP 404") || stderr.contains("404 File Not Found") || stderr.contains("404 Not Found")) && !stderr.contains("No commit found")
}

/// Runs the CLI and reads at most `cap` bytes of its output; past that it stops the CLI and says [`TOO_LARGE`].
#[cfg(test)]
fn fetch(cli: &str, args: &[String], cap: usize) -> Result<Option<Vec<u8>>, String> {
    fetch_on("", cli, args, cap)
}

/// The same, aimed at one server (see `crate::command`).
fn fetch_on(host: &str, cli: &str, args: &[String], cap: usize) -> Result<Option<Vec<u8>>, String> {
    let mut child = crate::command(host, cli)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{cli} could not run: {e}"))?;
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(format!("{cli} gave no output to read"));
    };
    // A CLI that never answers is killed with what it started, which closes the pipes and ends the read below.
    let deadline = crate::bounded::watch(&child, crate::CALL_LIMIT);
    // Drained on its own thread so a chatty CLI never blocks on a full pipe while stdout is read.
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        String::from_utf8_lossy(&text).into_owned()
    });
    let mut body = Vec::new();
    let read = stdout.take(cap as u64 + 1).read_to_end(&mut body);
    if body.len() > cap {
        let _ = child.kill();
        let _ = child.wait();
        return Err(TOO_LARGE.into());
    }
    let status = child.wait().map_err(|e| format!("{cli} could not run: {e}"))?;
    if deadline.fired() {
        return Err(format!("{cli} did not answer in {} seconds and was stopped", crate::CALL_LIMIT.as_secs()));
    }
    read.map_err(|e| format!("could not read {cli}'s output: {e}"))?;
    let stderr = errors.join().unwrap_or_default();
    if status.success() {
        Ok(Some(body))
    } else if not_found(&stderr) {
        Ok(None)
    } else {
        Err(format!("`{cli} {}` failed: {}", args.join(" "), stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_asks_for_raw_bytes_at_the_commit_with_each_path_segment_encoded() {
        let args = github_args("acme/api", "9f2c1a", "docs/read me#1?.md");
        assert!(args.iter().any(|a| a.contains("vnd.github.raw")), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("repos/acme/api/contents/docs/read%20me%231%3F.md?ref=9f2c1a"));
    }

    #[test]
    fn gitlab_encodes_the_project_and_the_whole_file_path_as_one_segment_each() {
        let args = gitlab_args("infra/aws/modules", "c0ffee", "src/icons/retry é.svg");
        assert_eq!(args, ["api", "projects/infra%2Faws%2Fmodules/repository/files/src%2Ficons%2Fretry%20%C3%A9.svg/raw?ref=c0ffee"]);
    }

    #[test]
    fn a_missing_file_is_told_from_an_unknown_commit_and_other_failures() {
        assert!(not_found("gh: Not Found (HTTP 404)"));
        assert!(not_found("glab: 404 File Not Found"));
        assert!(!not_found("gh: No commit found for the ref abc (HTTP 404)"));
        assert!(!not_found("gh: Bad credentials (HTTP 401)"));
    }

    #[test]
    fn output_past_the_cap_is_refused_and_output_within_it_is_kept() {
        let ten = ["-c".to_string(), "head -c 10 /dev/zero".to_string()];
        assert_eq!(fetch("sh", &ten, 10), Ok(Some(vec![0; 10])));
        assert_eq!(fetch("sh", &ten, 9), Err(TOO_LARGE.to_string()));
    }

    #[test]
    fn a_failing_cli_reports_its_error_unless_the_file_is_missing() {
        let failing = |message: &str| ["-c".to_string(), format!("echo '{message}' >&2; exit 1")];
        assert_eq!(fetch("sh", &failing("gh: Not Found (HTTP 404)"), 100), Ok(None));
        let error = fetch("sh", &failing("gh: Bad credentials (HTTP 401)"), 100).expect_err("a failure is an error");
        assert!(error.contains("Bad credentials"), "{error}");
    }
}
