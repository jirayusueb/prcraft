//! What went wrong with a forge call, sorted so a report can be read from its code alone.

use std::fmt;
use std::io;

/// Why a forge call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    Auth,
    Scope,
    RateLimit,
    Network,
    CliMissing,
    CliOld,
    Timeout,
    Refused,
    Other,
}

/// A failed forge call: the user reads `message`; `kind` picks the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeError {
    pub kind: FailKind,
    pub message: String,
}

impl ForgeError {
    pub fn new(kind: FailKind, message: impl Into<String>) -> Self {
        ForgeError { kind, message: message.into() }
    }

    /// The same failure with `prefix: ` in front of its message.
    #[must_use]
    pub fn context(self, prefix: impl fmt::Display) -> Self {
        ForgeError { kind: self.kind, message: format!("{prefix}: {}", self.message) }
    }

    /// Seconds a rate-limited server asked to wait, from a `Retry-After: <seconds>` line in the CLI's output.
    pub fn retry_after(&self) -> Option<u64> {
        let text = self.message.to_lowercase();
        let rest = &text[text.find("retry-after:")? + "retry-after:".len()..];
        let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    /// A CLI that did not start (`NotFound`) or ran out of time (`TimedOut`) said so through `error`.
    pub(crate) fn spawn(error: &io::Error, message: String) -> Self {
        let kind = match error.kind() {
            io::ErrorKind::NotFound => FailKind::CliMissing,
            io::ErrorKind::TimedOut => FailKind::Timeout,
            _ => FailKind::Other,
        };
        ForgeError { kind, message }
    }

    /// A CLI that ran and exited non-zero with `stderr`.
    pub(crate) fn exited(message: String, stderr: &str) -> Self {
        ForgeError { kind: classify(stderr), message }
    }
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ForgeError {}

impl From<String> for ForgeError {
    fn from(message: String) -> Self {
        ForgeError { kind: FailKind::Other, message }
    }
}

impl From<&str> for ForgeError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl AsRef<str> for ForgeError {
    fn as_ref(&self) -> &str {
        &self.message
    }
}

/// Whether `text` names HTTP status `code` the way `gh` ("HTTP 404:") and `glab` ("404 Not Found") print it.
fn status(text: &str, code: u16) -> bool {
    text.contains(&format!("http {code}")) || text.contains(&format!("{code} "))
}

/// The one list of stderr markers, in the order they win: a 403 that talks of a rate limit is a rate limit, one that talks of SAML
/// or a scope is a scope, and only a bare 403/404/422 is a refusal.
fn classify(stderr: &str) -> FailKind {
    let text = stderr.to_lowercase();
    let any = |marks: &[&str]| marks.iter().any(|m| text.contains(m));
    if any(&["rate limit", "api rate", "too many requests"]) || status(&text, 429) {
        FailKind::RateLimit
    } else if any(&["scope", "saml", " sso"]) {
        FailKind::Scope
    } else if any(&["not logged in", "authentication", "bad credentials", "unauthorized"]) || status(&text, 401) {
        FailKind::Auth
    } else if any(&["could not resolve host", "connection refused", "timed out", "network", "no such host"]) {
        FailKind::Network
    } else if any(&["unknown flag", "unknown command", "unknown shorthand flag"]) {
        FailKind::CliOld
    } else if status(&text, 403) || status(&text, 404) || status(&text, 422) {
        FailKind::Refused
    } else {
        FailKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_from_the_clis_is_sorted_by_kind() {
        let samples = [
            (FailKind::Auth, "To get started with GitHub CLI, please run:  gh auth login\nYou are not logged in to any GitHub hosts."),
            (FailKind::Auth, "gh: Bad credentials (HTTP 401)"),
            (FailKind::Auth, "glab: 401 Unauthorized"),
            (FailKind::Scope, "gh: Resource protected by organization SAML enforcement. You must grant your OAuth token access (HTTP 403)"),
            (FailKind::Scope, "error: your authentication token is missing required scopes [read:org]"),
            (FailKind::RateLimit, "gh: API rate limit exceeded for user ID 1. (HTTP 403)"),
            (FailKind::RateLimit, "glab: 429 Too Many Requests"),
            (FailKind::Network, "dial tcp: lookup github.com: no such host"),
            (FailKind::Network, "curl: (6) Could not resolve host: gitlab.acme.dev"),
            (FailKind::Network, "connect: connection refused"),
            (FailKind::CliOld, "unknown flag: --json"),
            (FailKind::Refused, "gh: Not Found (HTTP 404)"),
            (FailKind::Refused, "gh: Validation Failed (HTTP 422)"),
            (FailKind::Refused, "glab: 403 Forbidden"),
            (FailKind::Other, "something nobody planned for"),
        ];
        for (kind, stderr) in samples {
            assert_eq!(classify(stderr), kind, "{stderr}");
        }
    }

    #[test]
    fn a_cli_that_did_not_start_or_ran_out_of_time_is_told_apart() {
        assert_eq!(ForgeError::spawn(&io::Error::from(io::ErrorKind::NotFound), String::new()).kind, FailKind::CliMissing);
        assert_eq!(ForgeError::spawn(&io::Error::from(io::ErrorKind::TimedOut), String::new()).kind, FailKind::Timeout);
        assert_eq!(ForgeError::spawn(&io::Error::from(io::ErrorKind::PermissionDenied), String::new()).kind, FailKind::Other);
    }

    #[test]
    fn context_keeps_the_kind_and_prefixes_the_text() {
        let error = ForgeError::new(FailKind::RateLimit, "slow down").context("github.com");
        assert_eq!(error.kind, FailKind::RateLimit);
        assert_eq!(error.to_string(), "github.com: slow down");
        assert_eq!(ForgeError::from("plain").kind, FailKind::Other);
    }

    #[test]
    fn retry_after_is_read_from_the_output_or_absent() {
        let limited = |text: &str| ForgeError::new(FailKind::RateLimit, format!("`glab api x` failed: {text}"));
        assert_eq!(limited("429 Too Many Requests\nRetry-After: 120\n").retry_after(), Some(120));
        assert_eq!(limited("429 too many requests retry-after:7").retry_after(), Some(7));
        assert_eq!(limited("429 Too Many Requests").retry_after(), None);
    }
}
