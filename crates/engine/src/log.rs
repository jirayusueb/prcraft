//! One line per engine command in `~/Library/Logs/PR Craft/pr-craft.log`, so a failure a teammate reports can be told from text alone.
//! A line holds the time, the command id, where it pointed (forge, host, repo) and `ok` or the error code, and how long it took. It
//! never holds a param body, a token, a diff or a comment: only the three named params are copied.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use pr_craft_core::Utc;
use serde_json::Value;

use crate::CommandError;

const FILE: &str = "pr-craft.log";
const OLD: &str = "pr-craft.log.1";

/// The file grows to this size, then becomes `pr-craft.log.1` and a new one starts: two files hold at most twice this.
const LIMIT: u64 = 5 * 1024 * 1024;

/// How long a copied value may be; a longer one is cut.
const VALUE: usize = 100;

/// Where this process logs; unset until a binary calls [`install`], and nothing is written before that.
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// The log directory once [`install`] has run.
pub(crate) fn dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
}

/// Turns logging on for this process: commands log to `~/Library/Logs/PR Craft`, and a panic is logged before the earlier hook
/// runs. Both apps call it first thing in `main`; nothing else does, so tests never write to the real log.
pub fn install() {
    if let Some(home) = std::env::var_os("HOME")
        && DIR.set(PathBuf::from(home).join("Library/Logs/PR Craft")).is_ok()
    {
        install_panic_hook();
    }
}

fn now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A value from params as one word: whitespace and control characters (a newline would start a forged line) become `_`.
fn word(text: &str) -> String {
    text.chars().take(VALUE).map(|c| if c.is_whitespace() || c.is_control() { '_' } else { c }).collect()
}

/// `forge`, `host` and `repo` from the params, or from their `change`, as `key=value`.
fn target(params: &Value) -> String {
    let scope = params.get("change").unwrap_or(params);
    ["forge", "host", "repo"].iter().filter_map(|key| scope.get(key).and_then(Value::as_str).map(|value| format!(" {key}={}", word(value)))).collect()
}

/// The line for one finished command.
fn line(id: &str, params: &Value, result: &Result<Value, CommandError>, took: Duration) -> String {
    let outcome = result.as_ref().map_or_else(CommandError::code, |_| "ok");
    format!("{} {}{} {outcome} {}ms", now(), word(id), target(params), took.as_millis())
}

/// Adds `text` and a newline to the log in `dir`. The current file moves to `.1` first when it has reached `limit`. A failure is
/// returned for tests and ignored by callers: a command never fails because its log did.
fn append_in(dir: &Path, text: &str, limit: u64) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let file = dir.join(FILE);
    if fs::metadata(&file).is_ok_and(|m| m.len() >= limit) {
        fs::rename(&file, dir.join(OLD))?;
    }
    writeln!(OpenOptions::new().create(true).append(true).open(file)?, "{text}")
}

/// Logs one finished command in `dir`.
pub(crate) fn record_in(dir: &Path, id: &str, params: &Value, result: &Result<Value, CommandError>, took: Duration) {
    let _ = append_in(dir, &line(id, params, result, took), LIMIT);
}

/// Logs one free-form line (the engine's own housekeeping) when [`install`] has run.
pub(crate) fn note(text: &str) {
    if let Some(dir) = dir() {
        let _ = append_in(dir, &format!("{} {text}", now()), LIMIT);
    }
}

/// Logs one finished command when [`install`] has run.
pub(crate) fn record(id: &str, params: &Value, result: &Result<Value, CommandError>, took: Duration) {
    if let Some(dir) = dir() {
        record_in(dir, id, params, result, took);
    }
}

/// The last `count` lines of the log in `dir`, oldest first, reaching into the rotated file when the current one is short.
pub(crate) fn tail_in(dir: &Path, count: usize) -> Vec<String> {
    let read = |name: &str| fs::read_to_string(dir.join(name)).unwrap_or_default();
    let mut lines: Vec<String> = read(OLD).lines().map(String::from).collect();
    lines.extend(read(FILE).lines().map(String::from));
    lines.split_off(lines.len().saturating_sub(count))
}

/// Writes `E-PANIC`, the message, the place and a backtrace to the log, then runs the hook that was set before.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = DIR.get() {
            let message =
                info.payload().downcast_ref::<&str>().map(|s| (*s).to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned());
            let place = info.location().map_or_else(String::new, |l| format!(" at {}:{}", l.file(), l.line()));
            let backtrace = std::backtrace::Backtrace::force_capture();
            let text = format!("{} E-PANIC {}{place}\n{backtrace}", now(), message.unwrap_or_else(|| "panic".into()));
            let _ = append_in(dir, text.trim_end(), LIMIT);
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pr-craft-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_command_leaves_one_line_with_its_id_and_code_and_never_its_body() {
        let dir = temp_dir("line");
        let params = json!({ "change": { "forge": "github", "repo": "acme/api", "number": 4 }, "body": "SECRET-COMMENT", "title": "SECRET-TITLE" });
        record_in(&dir, "thread.reply", &params, &Err(CommandError::Invalid("no".into())), Duration::from_millis(7));
        record_in(&dir, "review.open", &params, &Ok(json!({ "diff": "SECRET-DIFF" })), Duration::from_millis(3));
        let text = fs::read_to_string(dir.join(FILE)).expect("log");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains(" thread.reply forge=github repo=acme/api E-INVALID 7ms"), "{}", lines[0]);
        assert!(lines[1].contains(" review.open ") && lines[1].contains(" ok "), "{}", lines[1]);
        assert!(!text.contains("SECRET"), "{text}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_param_cannot_start_a_forged_line() {
        let dir = temp_dir("forged");
        record_in(&dir, "review.open", &json!({ "repo": "a\n2026 E-FAKE b" }), &Ok(Value::Null), Duration::ZERO);
        assert_eq!(fs::read_to_string(dir.join(FILE)).expect("log").lines().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_log_moves_to_dot_1_and_a_new_one_starts() {
        let dir = temp_dir("rotate");
        append_in(&dir, "first", 5).expect("write");
        append_in(&dir, "second", 5).expect("write");
        append_in(&dir, "third", 5).expect("write");
        assert_eq!(fs::read_to_string(dir.join(FILE)).expect("current"), "third\n");
        assert_eq!(fs::read_to_string(dir.join(OLD)).expect("old"), "second\n", "one old file is kept");
        assert_eq!(tail_in(&dir, 5), ["second", "third"]);
        assert_eq!(tail_in(&dir, 1), ["third"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
