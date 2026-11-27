//! Your changes: the open changes the user wrote, and what the forge says about each (checks, reviews, merge state). Pure words
//! over what the forge layer reads. No I/O. Babysit (an agent that watches a change) is not built, so nothing here models it.

use serde::{Deserialize, Serialize};

use crate::{Change, DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Passing,
    Failing,
    Running,
    /// Ran and chose to do nothing (neutral, skipped). Counts as passing.
    Skipped,
}

/// One check run or pipeline job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// What `checks.log` and `checks.rerun` take back: `<run>/<job>` on GitHub, the job id on GitLab.
    pub id: String,
    pub name: String,
    /// The workflow (GitHub) or stage (GitLab) it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub state: CheckState,
    /// How long it ran, when the forge says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A log can be read for it: a GitHub Actions job or a GitLab job. An external status has none.
    pub has_log: bool,
}

impl Check {
    /// `41s`, `1m 12s`, `1h 03m`.
    pub fn took(&self) -> Option<String> {
        let s = self.seconds?;
        Some(match s {
            0..60 => format!("{s}s"),
            60..3600 => format!("{}m {:02}s", s / 60, s % 60),
            _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
        })
    }
}

/// The checks of a change at its head commit, as the list reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Checks {
    /// `None`: the change has no checks, or the forge did not say.
    pub state: Option<CheckState>,
    /// How many checks the forge holds; `failed + running + passed` is how many were read.
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub running: u32,
    /// Names of the failing ones that were read.
    pub failed_names: Vec<String>,
}

impl Checks {
    /// `14 of 14 checks`, or `13 of 14 checks passing` while some fail or run.
    pub fn counts(&self) -> String {
        match (self.state, self.total) {
            (None, _) | (_, 0) => String::new(),
            (Some(CheckState::Passing), n) => format!("{n} of {n} checks"),
            (_, n) => format!("{} of {n} checks passing", self.passed),
        }
    }

    /// The summary of a list of checks that was read whole.
    pub fn of(list: &[Check]) -> Checks {
        let count = |state| list.iter().filter(|c| c.state == state).count() as u32;
        let (failed, running) = (count(CheckState::Failing), count(CheckState::Running));
        let state = match list.len() {
            0 => None,
            _ if failed > 0 => Some(CheckState::Failing),
            _ if running > 0 => Some(CheckState::Running),
            _ => Some(CheckState::Passing),
        };
        Checks {
            state,
            total: list.len() as u32,
            passed: list.len() as u32 - failed - running,
            failed,
            running,
            failed_names: list.iter().filter(|c| c.state == CheckState::Failing).map(|c| c.name.clone()).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approved,
    ChangesRequested,
    /// Reviews are required and none counts yet.
    ReviewRequired,
    /// The forge asks for no review of this change, or does not say.
    None,
}

/// Whether the forge would merge the change now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    Ready,
    Behind,
    Conflicts,
    /// Something the forge names stops it; see `Mine::merge_note`.
    Blocked,
    /// The forge has not worked it out yet, or does not say.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesRequest {
    pub by: String,
    pub at: Option<DateTime<Utc>>,
}

/// One of the user's own open changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mine {
    pub change: Change,
    /// `None` when the forge's size was not read (`unread` says so).
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub head_branch: String,
    /// The head commit, so a merge can name the commit the user saw.
    pub head_sha: String,
    pub base_branch: String,
    /// How many commits, when read.
    pub commits: Option<u32>,
    /// Asked to review, and those who reviewed.
    pub reviewers: Vec<String>,
    /// Of those, the ones still asked to review: the only ones a change of reviewers can take off.
    pub requested: Vec<String>,
    pub decision: Decision,
    pub approvals: u32,
    /// How many approvals the forge requires, when it says.
    pub approvals_required: Option<u32>,
    pub changes_requested: Vec<ChangesRequest>,
    pub threads_open: u32,
    /// Open threads whose last comment is somebody else's: the ones that wait on the user.
    pub threads_waiting: u32,
    pub checks: Checks,
    pub merge: MergeState,
    /// What the forge says stops the merge, in its words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_note: Option<String>,
    /// Merge when ready is on.
    pub auto_merge: bool,
    pub labels: Vec<String>,
    /// What could not be read for this change, so a missing number is never mistaken for zero.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Failing, conflicting, changes requested.
    Bad,
    /// Waiting on something.
    Warn,
    /// Approved, ready.
    Good,
    Quiet,
}

/// The words of a row's status line and how loud they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub tone: Tone,
}

fn plural(n: u32, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

impl Mine {
    /// A reviewer is waiting on the user: they asked for changes, or an open thread ends with their comment.
    pub fn waiting_on_you(&self) -> bool {
        !self.change.draft && (self.decision == Decision::ChangesRequested || self.threads_waiting > 0)
    }

    /// `+9 −3`, or nothing when the size was not read.
    pub fn size(&self) -> Option<(u64, u64)> {
        Some((self.additions?, self.deletions?))
    }

    /// The first thing the user should know, in the order a reader would act on it.
    pub fn status(&self) -> Status {
        let mut status = self.first_status();
        // An armed Merge when ready is never hidden behind a louder line: it will merge when the blocker clears.
        if self.auto_merge && !self.change.draft && !status.text.to_lowercase().contains("waiting to merge") {
            status.text.push_str(" · merge when ready is on");
        }
        status
    }

    fn first_status(&self) -> Status {
        let say = |text: String, tone| Status { text, tone };
        let base = &self.base_branch;
        if self.change.draft {
            let who =
                if self.reviewers.is_empty() { "no reviewers yet".to_string() } else { plural(self.reviewers.len() as u32, "reviewer", "reviewers") };
            return say(format!("Draft · {who}"), Tone::Quiet);
        }
        if self.merge == MergeState::Conflicts {
            return say(format!("Conflicts with {base}"), Tone::Bad);
        }
        if self.decision == Decision::ChangesRequested {
            let open = if self.threads_open > 0 { format!(" · {} open", plural(self.threads_open, "thread", "threads")) } else { String::new() };
            return say(format!("Changes requested{open}"), Tone::Bad);
        }
        if self.checks.state == Some(CheckState::Failing) {
            let which = match self.checks.failed_names.as_slice() {
                [one] => format!(" · {one}"),
                _ if self.checks.failed > 1 => format!(" · {} failed", self.checks.failed),
                _ => String::new(),
            };
            return say(format!("Checks failed{which}"), Tone::Bad);
        }
        if self.threads_waiting > 0 {
            return say(plural(self.threads_waiting, "thread waits on you", "threads wait on you"), Tone::Warn);
        }
        let running = self.checks.state == Some(CheckState::Running);
        if self.decision == Decision::Approved {
            let rest = match (self.auto_merge, self.merge) {
                (true, _) => "waiting to merge".to_string(),
                (_, MergeState::Behind) => format!("behind {base}"),
                (_, _) if running => "checks running".to_string(),
                (_, MergeState::Ready) => "ready to merge".to_string(),
                (_, MergeState::Blocked) => "merge is blocked".to_string(),
                (_, _) => "merge state not known yet".to_string(),
            };
            let tone = if rest == "ready to merge" || rest == "waiting to merge" { Tone::Good } else { Tone::Warn };
            return say(format!("Approved · {rest}"), tone);
        }
        if self.merge == MergeState::Behind {
            return say(format!("Behind {base}"), Tone::Warn);
        }
        if running {
            return say("Checks running".into(), Tone::Quiet);
        }
        if self.auto_merge {
            return say("Waiting to merge".into(), Tone::Good);
        }
        let review = if self.reviewers.is_empty() { "no reviewers yet" } else { "waiting for review" };
        match self.checks.state {
            Some(CheckState::Passing | CheckState::Skipped) => say(format!("Checks passing · {review}"), Tone::Good),
            _ => say(format!("Open · {review}"), Tone::Quiet),
        }
    }
}

/// How a change is merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    pub const ALL: [MergeMethod; 3] = [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase];

    /// The button words: `Squash and merge`.
    pub fn label(self) -> &'static str {
        match self {
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Merge => "Merge commit",
            MergeMethod::Rebase => "Rebase and merge",
        }
    }

    /// What the forge does, for a confirm step that names it exactly.
    pub fn does(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squashes every commit into one and merges it",
            MergeMethod::Merge => "merges with a merge commit",
            MergeMethod::Rebase => "rebases every commit onto the base and merges them",
        }
    }
}

/// The merge queue of the base branch, when the forge reports one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeQueue {
    /// Changes already queued.
    pub ahead: u32,
    /// The forge's estimate, in seconds, when it gives one.
    pub estimate_secs: Option<u64>,
    /// This change is in the queue already.
    pub queued: bool,
}

/// An issue a merge would close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedIssue {
    pub number: u64,
    pub title: String,
}

/// One change read in full, for its reading pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MineDetail {
    pub mine: Mine,
    /// Every check read; empty with `checks_error` set when they could not be.
    pub checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_error: Option<String>,
    /// `Some` only when the forge says the base branch has a merge queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<MergeQueue>,
    /// Issues the forge says merging closes. Empty when none or when the forge does not say.
    pub closes: Vec<LinkedIssue>,
    /// The methods the repository allows.
    pub methods: Vec<MergeMethod>,
    /// What the forge proposes as the commit message for a squash.
    pub message: String,
    /// Why the queue could not be read, when it could not (so its absence is not read as "no queue").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_error: Option<String>,
}

/// One line of a job log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogLine {
    pub text: String,
    /// The line reads like a failure; the log view tints it.
    pub failed: bool,
}

/// A job log read from the forge, cut to its last [`LOG_LINES`] lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobLog {
    pub lines: Vec<LogLine>,
    /// How many lines were read. When `from_cut` it counts only the end of the log that was kept, so the log has more.
    pub total: usize,
    /// Some lines were left out to keep the log small.
    pub truncated: bool,
    /// The text read was only the end of a longer log: `total` is a lower bound and line numbers count from where it starts.
    pub from_cut: bool,
}

/// The most log lines kept.
pub const LOG_LINES: usize = 2000;

/// The most characters kept of one log line.
pub const MAX_LINE_CHARS: usize = 2000;

/// Whether `lower` holds `pattern` not glued to a digit or letter before it (`0 failed` is calm, `10 failed` is not).
fn holds(lower: &str, pattern: &str) -> bool {
    lower.match_indices(pattern).any(|(at, _)| !lower.get(..at).and_then(|s| s.chars().next_back()).is_some_and(char::is_alphanumeric))
}

/// Whether a log line reads like a failure. shortcut: a keyword match, not a parse of any tool's output; it will tint a line that
/// merely mentions an error, and will miss a failure phrased some other way.
pub fn failure_line(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.starts_with("##[error]") {
        return true;
    }
    // A test runner's passing line names its test (`✓ rejects an error`); a git line names a branch.
    let line = lower.trim_start();
    let first: String = line.chars().take_while(|c| c.is_alphabetic()).collect();
    let passing = ["✓", "✔", "* [new"].iter().any(|p| line.starts_with(p)) || matches!(first.as_str(), "pass" | "passed" | "passing" | "ok");
    let calm = ["0 failed", "0 errors", "no errors", "no error", "0 failures", "error_count: 0"];
    let unwell = ["error", "fail", "panic", "fatal", "exception", "exit code 1", "✗", "✕"];
    !passing && !calm.iter().any(|c| holds(&lower, c)) && unwell.iter().any(|k| lower.contains(k))
}

/// Drops every terminal control sequence (CSI colours and cursor moves, OSC, DCS, APC, PM and SOS strings, a bare ESC) and every
/// other control character but the tab, and a leading byte-order mark.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() || c == '\t' {
                out.push(c);
            }
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            Some(']' | 'P' | '_' | '^' | 'X') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' || (next == '\u{1b}' && chars.peek() == Some(&'\\')) {
                        if next != '\u{7}' {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            Some(next) if ('\u{40}'..='\u{5f}').contains(&next) => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// A terminal draws what follows a carriage return over what came before it, and GitLab starts every line of a section with
/// `section_start:…\r`, so the text of a line is its last non-empty part. `None` for a line that is only a section marker.
fn visible(line: &str) -> Option<String> {
    let text = line.split('\r').map(plain).rfind(|part| !part.is_empty()).unwrap_or_default();
    let only_marker = text.starts_with("section_start:") || text.starts_with("section_end:") || (text.is_empty() && line.contains("section_"));
    (!only_marker).then_some(text)
}

/// The text of one `gh run view --log-failed` line: `<job>\t<step>\t<timestamp> <text>`. A line without two tabs is kept whole.
fn github_text(line: &str) -> &str {
    let mut parts = line.splitn(3, '\t');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(_), Some(_), Some(rest)) => {
            rest.split_once(' ').filter(|(stamp, _)| stamp.ends_with('Z') && stamp.contains('T')).map_or(rest, |(_, text)| text)
        }
        _ => line,
    }
}

/// Reads a log. `github` strips the job and step columns and the timestamp `gh run view --log-failed` puts on every line.
pub fn parse_log(raw: &str, github: bool) -> JobLog {
    parse_log_cut(raw, github, false)
}

/// [`parse_log`] for text that is only the end of a longer log (`cut`): its first line may be a part of one, so it is dropped when a
/// later line exists, and the result says the start was left out. Only the last [`LOG_LINES`] lines are kept, each cut to
/// [`MAX_LINE_CHARS`]. A tail that is one unbroken line is kept (cut), never reported as empty.
pub fn parse_log_cut(raw: &str, github: bool, cut: bool) -> JobLog {
    let mut kept: std::collections::VecDeque<String> = std::collections::VecDeque::with_capacity(LOG_LINES + 1);
    let mut total = 0usize;
    let skip = usize::from(cut && raw.trim_end_matches(['\r', '\n']).contains('\n'));
    for line in raw.lines().skip(skip) {
        let Some(text) = visible(if github { github_text(line) } else { line }) else { continue };
        total += 1;
        if kept.len() == LOG_LINES {
            kept.pop_front();
        }
        kept.push_back(text.chars().take(MAX_LINE_CHARS).collect());
    }
    let truncated = cut || total > kept.len();
    let lines = kept.into_iter().map(|text| LogLine { failed: failure_line(&text), text }).collect();
    JobLog { lines, total, truncated, from_cut: cut }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Forge;

    fn mine() -> Mine {
        Mine {
            change: Change {
                forge: Forge::GitHub,
                repo: "acme/api".into(),
                number: 490,
                title: "Use crypto/rand".into(),
                author: "maria".into(),
                url: "https://github.com/acme/api/pull/490".into(),
                host: None,
                updated_at: Utc::now(),
                draft: false,
                stack: None,
            },
            additions: Some(9),
            deletions: Some(3),
            head_branch: "maria/jitter".into(),
            head_sha: "a1b2c3d".into(),
            base_branch: "main".into(),
            commits: Some(2),
            reviewers: vec!["nina.k".into()],
            requested: vec!["nina.k".into()],
            decision: Decision::None,
            approvals: 0,
            approvals_required: None,
            changes_requested: Vec::new(),
            threads_open: 0,
            threads_waiting: 0,
            checks: Checks::default(),
            merge: MergeState::Ready,
            merge_note: None,
            auto_merge: false,
            labels: Vec::new(),
            unread: Vec::new(),
        }
    }

    #[test]
    fn the_status_line_puts_what_blocks_the_change_before_what_is_fine() {
        let mut m = mine();
        m.checks = Checks { state: Some(CheckState::Failing), total: 14, passed: 13, failed: 1, running: 0, failed_names: vec!["test:unit".into()] };
        m.decision = Decision::Approved;
        assert_eq!(m.status(), Status { text: "Checks failed · test:unit".into(), tone: Tone::Bad }, "an approval does not hide red checks");
        m.merge = MergeState::Conflicts;
        assert_eq!(m.status().text, "Conflicts with main", "a conflict outranks failing checks");
        m.merge = MergeState::Ready;
        m.checks = Checks { state: Some(CheckState::Passing), total: 14, passed: 14, ..Checks::default() };
        assert_eq!(m.status().text, "Approved · ready to merge");
        m.auto_merge = true;
        assert_eq!(m.status().text, "Approved · waiting to merge");
        m.change.draft = true;
        m.reviewers.clear();
        assert_eq!(m.status().text, "Draft · no reviewers yet", "a draft is a draft before anything else");
        assert!(!m.waiting_on_you(), "nobody is asked to look at a draft");
    }

    #[test]
    fn a_reviewer_who_waits_marks_the_change_and_a_quiet_one_does_not() {
        let mut m = mine();
        assert!(!m.waiting_on_you());
        m.decision = Decision::ChangesRequested;
        m.threads_open = 2;
        assert!(m.waiting_on_you());
        assert_eq!(m.status().text, "Changes requested · 2 threads open");
        m.decision = Decision::None;
        m.threads_waiting = 1;
        assert!(m.waiting_on_you(), "an open thread that ends with the reviewer's comment waits on the author");
        assert_eq!(m.status().text, "1 thread waits on you");
    }

    #[test]
    fn a_change_that_is_behind_or_running_says_so_and_never_claims_it_can_merge() {
        let mut m = mine();
        m.decision = Decision::Approved;
        m.merge = MergeState::Behind;
        assert_eq!(m.status().text, "Approved · behind main");
        m.merge = MergeState::Unknown;
        assert_eq!(m.status().text, "Approved · merge state not known yet");
        m.merge = MergeState::Ready;
        m.checks.state = Some(CheckState::Running);
        assert_eq!(m.status().text, "Approved · checks running");
    }

    #[test]
    fn a_github_failed_log_loses_its_columns_and_marks_the_failing_lines() {
        let raw = "\u{feff}Validate\tRun tests\t2026-10-21T10:54:04.2103656Z go test ./...\n\
                   Validate\tRun tests\t2026-10-21T10:54:05.0000000Z \u{1b}[31;1m--- FAIL: TestRetry\u{1b}[0m\n\
                   Validate\tRun tests\t2026-10-21T10:54:06.0000000Z ok  \tgithub.com/acme/api\t0.1s\n\
                   Validate\tRun tests\t2026-10-21T10:54:07.0000000Z ##[error]Process completed with exit code 1.\n";
        let log = parse_log(raw, true);
        let texts: Vec<&str> = log.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            ["go test ./...", "--- FAIL: TestRetry", "ok  \tgithub.com/acme/api\t0.1s", "##[error]Process completed with exit code 1."]
        );
        assert_eq!(log.lines.iter().map(|l| l.failed).collect::<Vec<_>>(), [false, true, false, true]);
        assert!(!log.truncated);
    }

    #[test]
    fn a_long_log_keeps_its_end_and_says_it_was_cut() {
        let raw: String = (0..LOG_LINES + 40).map(|n| format!("line {n}\n")).collect();
        let log = parse_log(&raw, false);
        assert_eq!(log.total, LOG_LINES + 40);
        assert_eq!(log.lines.len(), LOG_LINES);
        assert!(log.truncated, "the reader is told lines were left out");
        assert_eq!(
            log.lines.last().map(|l| l.text.as_str()),
            Some(format!("line {}", LOG_LINES + 39).as_str()),
            "the failure is at the end, so the end stays"
        );
    }

    #[test]
    fn a_gitlab_trace_drops_its_section_markers_and_colours() {
        let raw = "section_start:1700000000:step_script\r\u{1b}[0K\u{1b}[32;1m$ go test\u{1b}[0;m\nFAIL\tgithub.com/acme/api\t0.4s\nsection_end:1700000001:step_script\r\u{1b}[0K\n";
        let log = parse_log(raw, false);
        assert_eq!(log.lines.len(), 2, "{log:?}");
        assert_eq!(log.lines[0].text, "$ go test");
        assert!(log.lines[1].failed);
    }

    #[test]
    fn a_passing_test_that_names_an_error_is_not_tinted_and_a_failing_one_is() {
        assert!(!failure_line("  ✓  unit-node  src/utils/api-error.spec.ts (9 tests) 38ms"));
        assert!(!failure_line("Tests: 0 failed, 12 passed"));
        assert!(failure_line("  ❯  unit-dom  math.spec.tsx (4 tests | 1 failed) 12338ms"));
        assert!(failure_line("FAIL  github.com/acme/api/internal/webhooks  0.412s"));
    }

    #[test]
    fn a_summary_of_many_failures_is_tinted_and_a_summary_of_none_is_not() {
        for line in ["Tests: 10 failed, 20 passed", "20 failed, 5 passed", "aborting due to 10 errors", "100 failures"] {
            assert!(failure_line(line), "{line}");
        }
        assert!(!failure_line("Tests: 0 failed, 20 passed"));
        assert!(failure_line("Password check failed"), "a word that starts like pass is not a passing line");
    }

    #[test]
    fn every_terminal_sequence_and_control_character_is_dropped_and_a_long_line_is_cut() {
        let raw = "a\u{1b}]52;c;aGVsbG8=\u{7}b\u{1b}]0;title\u{1b}\\c\u{1b}Pdcs\u{1b}\\d\u{1b}[31;1me\u{1b}[0m\u{7}f\u{8}g\u{1b}h\u{85}i";
        assert_eq!(parse_log(raw, false).lines[0].text, "abcdefghi");
        let long = "x".repeat(MAX_LINE_CHARS * 5);
        assert_eq!(parse_log(&long, false).lines[0].text.chars().count(), MAX_LINE_CHARS);
    }

    #[test]
    fn the_end_of_a_cut_log_drops_its_first_part_line_and_says_so() {
        let log = parse_log_cut("rtial line\nsecond\nthird", false, true);
        assert!(log.truncated);
        assert_eq!(log.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["second", "third"]);
        assert!(log.from_cut, "its total is a lower bound, and its numbers count from the cut");
    }

    #[test]
    fn a_cut_log_that_is_one_huge_line_is_shown_cut_and_never_as_empty() {
        let log = parse_log_cut(&"x".repeat(MAX_LINE_CHARS * 3), false, true);
        assert_eq!(log.lines.len(), 1);
        assert_eq!(log.lines[0].text.chars().count(), MAX_LINE_CHARS);
        assert!(log.truncated);
    }

    #[test]
    fn a_cut_log_that_is_one_huge_line_with_a_trailing_newline_is_not_empty() {
        let log = parse_log_cut(&format!("{}\n", "x".repeat(MAX_LINE_CHARS * 3)), false, true);
        assert_eq!(log.lines.len(), 1, "the one line there is stays");
        let two = parse_log_cut("partial\nwhole\n", false, true);
        assert_eq!(two.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["whole"], "a part-line before a whole one is dropped");
    }

    #[test]
    fn an_armed_merge_when_ready_shows_in_the_status_whatever_blocks_it() {
        let mut m = mine();
        m.auto_merge = true;
        m.merge = MergeState::Conflicts;
        assert_eq!(m.status().text, "Conflicts with main · merge when ready is on");
    }
}
