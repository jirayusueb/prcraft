//! Runs one agent: starts the engine CLI, follows its stream-json output and turns its answer into AI drafts.
//!
//! The prompt asks the engine to answer with one JSON array, `[{"path": "src/a.rs", "line": "L42", "body": "..."}]`, where
//! `line` is the tag printed in front of a diff line: `L<n>` for an added or context line (numbered in the new file) and
//! `O<n>` for a removed line (numbered in the old file). A finding whose path is not a file of the run or whose line is not
//! a line of its diff is dropped and counted in [`Run::dropped`]; it is never moved to a nearby line.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pr_craft_core::Utc;
use pr_craft_core::agent::EngineSettings;
use pr_craft_core::diff::{FileDiff, LineKind};
use pr_craft_core::redact::{MASK, ReadRules, redact};
use pr_craft_core::review::LineRef;
use pr_craft_core::run::{AiDraft, DraftState, Finding, NodeStatus, Run, RunState, Skipped, stuck};
use serde_json::Value;

use crate::store::{self, Signal};

/// The arguments after the program name. The prompt goes in on standard input, which `claude -p` reads, so a large diff
/// never meets the argument size limit and never shows in `ps`.
///
/// With the diff-only read scope the prompt holds everything the engine may see, so it gets nothing else: `--tools ""`
/// removes every built-in tool, `--strict-mcp-config` (with no `--mcp-config`) loads no MCP server, `--restricted` ignores
/// user, project and local settings, and `--safe-mode` turns off CLAUDE.md, skills, plugins and hooks. `dontAsk` denies
/// anything that would still prompt, and `--no-session-persistence` keeps the prompt, which holds diff text, out of Claude
/// Code's session files. `--include-partial-messages` makes the stream print as the model writes, so a long answer is not
/// mistaken for a stuck run.
pub const CLAUDE_ARGS: [&str; 13] = [
    "-p",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--tools",
    "",
    "--strict-mcp-config",
    "--restricted",
    "--safe-mode",
    "--permission-mode",
    "dontAsk",
    "--no-session-persistence",
];

const ANSWER_FORMAT: &str = "Each diff line below reads `<marker><tag>| <text>`, and its tag names the line: ` L42` is a context line and `+L42` an added \
line (both numbered in the new file), `-O42` is a removed line (numbered in the old file).\n\
Report only problems on lines the diff shows. You cannot change or post anything; a program reads your answer.\n\
Answer with one JSON array and nothing else: [{\"path\": \"src/a.rs\", \"line\": \"L42\", \"body\": \"what can go wrong, and the smallest fix\"}]. \
`line` is the tag printed in front of the line you mean, as a string: \"L42\", or \"O42\" for a removed line. Answer [] when there is nothing to report.";

/// What goes into one prompt.
pub struct PromptInput<'a> {
    /// The agent's prompt; `{pr_title}`, `{files}`, `{diff}` and `{base_branch}` are filled in.
    pub template: &'a str,
    pub reference: &'a str,
    pub title: &'a str,
    pub base_branch: &'a str,
    pub files: &'a [FileDiff],
    pub rules: &'a ReadRules,
}

pub struct Prompt {
    pub text: String,
    /// The files whose diff is in the text.
    pub sent: Vec<String>,
    /// Files dropped whole by a never-send pattern.
    pub skipped: Vec<Skipped>,
    /// Secrets masked.
    pub redactions: usize,
}

/// The one place a prompt is built: files the read rules withhold are dropped and secrets masked before any text is joined.
pub fn build_prompt(input: &PromptInput) -> Prompt {
    let rules = input.rules;
    let (mut sent, mut skipped, mut redactions, mut diff) = (Vec::new(), Vec::new(), 0, String::new());
    for file in input.files {
        if rules.withheld(&file.path) || file.old_path.as_deref().is_some_and(|old| rules.withheld(old)) {
            skipped.push(Skipped { path: file.path.clone(), reason: "matches a never-send pattern".into() });
            continue;
        }
        let (text, found) = render_file(file, rules.redact_secrets);
        redactions += found;
        diff.push_str(&text);
        sent.push(file.path.clone());
    }
    let mut scrub = |text: &str| {
        if !rules.redact_secrets {
            return text.to_string();
        }
        let masked = redact(text);
        redactions += masked.found;
        masked.text
    };
    let (title, base_branch, reference) = (scrub(input.title), scrub(input.base_branch), scrub(input.reference));
    let files = sent.join(", ");
    let filled = input.template.replace("{pr_title}", &title).replace("{base_branch}", &base_branch).replace("{files}", &files);
    let text = if filled.contains("{diff}") {
        format!("{}\n\n---\nChange under review: {reference} {title}\n{ANSWER_FORMAT}", filled.replace("{diff}", &diff))
    } else {
        format!("{filled}\n\n---\nChange under review: {reference} {title}\n{ANSWER_FORMAT}\n\nFiles in your scope: {files}\n\nDiff:\n{diff}")
    };
    Prompt { text, sent, skipped, redactions }
}

/// One file as `<marker><tag>| <text>` lines. Hunk headers and line texts go through the redactor as one text, so a
/// private key that spans lines is seen whole.
fn render_file(file: &FileDiff, redact_secrets: bool) -> (String, usize) {
    let mut texts: Vec<&str> = Vec::new();
    for hunk in &file.hunks {
        texts.push(&hunk.header);
        texts.extend(hunk.lines.iter().map(|line| line.text.as_str()));
    }
    let joined = texts.join("\n");
    let (cleaned, found) = if redact_secrets {
        let masked = redact(&joined);
        (masked.text, masked.found)
    } else {
        (joined, 0)
    };
    let mut cleaned = cleaned.split('\n');
    let mut out = format!("=== {}", file.path);
    if let Some(old) = &file.old_path {
        let _ = write!(out, " (renamed from {old})");
    }
    out.push('\n');
    for hunk in &file.hunks {
        out.push_str(cleaned.next().unwrap_or(MASK));
        out.push('\n');
        for line in &hunk.lines {
            let (marker, tag, number) = match line.kind {
                LineKind::Added => ('+', 'L', line.new),
                LineKind::Removed => ('-', 'O', line.old),
                LineKind::Context => (' ', 'L', line.new),
            };
            let _ = writeln!(out, "{marker}{tag}{}| {}", number.unwrap_or(0), cleaned.next().unwrap_or(MASK));
        }
    }
    (out, found)
}

/// The findings in the engine's answer, and how many entries were dropped. `None` when the answer holds no JSON array.
pub fn parse_findings(answer: &str, files: &[FileDiff], agent: &str) -> Option<(Vec<Finding>, u32)> {
    let items = answer.match_indices('[').find_map(|(at, _)| {
        let mut stream = serde_json::Deserializer::from_str(answer.get(at..)?).into_iter::<Value>();
        match stream.next()? {
            Ok(Value::Array(items)) if items.iter().all(Value::is_object) => Some(items),
            _ => None,
        }
    })?;
    let (mut found, mut dropped) = (Vec::new(), 0);
    for item in &items {
        match finding_of(item, files, agent) {
            Some(finding) => found.push(finding),
            None => dropped += 1,
        }
    }
    Some((found, dropped))
}

/// Which line a finding names.
#[derive(Clone, Copy)]
enum Named {
    /// `L42`: an added or context line, by its number in the new file.
    New(u32),
    /// `O42`: a removed line, by its number in the old file.
    Old(u32),
    /// A bare number, which both a new-side and a removed line can carry.
    Bare(u32),
}

fn named(item: &Value) -> Option<Named> {
    let line = item.get("line")?;
    let old_side = item.get("side").and_then(Value::as_str) == Some("old");
    let bare = |n: u32| if old_side { Named::Old(n) } else { Named::Bare(n) };
    if let Some(number) = line.as_u64() {
        return u32::try_from(number).ok().map(bare);
    }
    let text = line.as_str()?.trim().trim_start_matches(['+', '-', ' ']);
    let mut chars = text.chars();
    match chars.next()? {
        'L' | 'l' => chars.as_str().parse().ok().map(Named::New),
        'O' | 'o' => chars.as_str().parse().ok().map(Named::Old),
        _ => text.parse().ok().map(bare),
    }
}

fn finding_of(item: &Value, files: &[FileDiff], agent: &str) -> Option<Finding> {
    let path = item.get("path")?.as_str()?;
    let body = item.get("body")?.as_str()?.trim();
    if body.is_empty() {
        return None;
    }
    let file = files.iter().find(|file| file.path == path)?;
    let lines = || file.hunks.iter().flat_map(|hunk| &hunk.lines);
    let on_new = |n: u32| lines().find(|line| line.kind != LineKind::Removed && line.new == Some(n));
    let on_old = |n: u32| lines().find(|line| line.kind == LineKind::Removed && line.old == Some(n));
    let hit = match named(item)? {
        Named::New(n) => on_new(n)?,
        Named::Old(n) => on_old(n)?,
        // Both sides carry this number: a guess could land the comment on the wrong line, so it is dropped.
        Named::Bare(n) => match (on_new(n), on_old(n)) {
            (Some(line), None) | (None, Some(line)) => line,
            _ => return None,
        },
    };
    Some(Finding {
        path: file.path.clone(),
        old_path: file.old_path.clone(),
        line: LineRef { old: hit.old, new: hit.new },
        body: body.to_string(),
        agent: agent.to_string(),
    })
}

/// What one line of the engine's stream-json output tells us.
#[derive(Default)]
struct Seen {
    summary: Option<String>,
    /// The text of a finished assistant message.
    assistant_text: Option<String>,
    result: Option<Answer>,
}

struct Answer {
    ok: bool,
    text: String,
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn parse_event(line: &str) -> Seen {
    let mut seen = Seen::default();
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        seen.summary = Some(clip(line, 160)).filter(|text| !text.trim().is_empty());
        return seen;
    };
    match event.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let text: String = event
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text")?.as_str())
                .collect();
            seen.summary = text.lines().find(|l| !l.trim().is_empty()).map(|l| clip(l.trim(), 160));
            seen.assistant_text = Some(text).filter(|text| !text.trim().is_empty());
        }
        Some("result") => {
            let failed = event.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                || event.get("subtype").and_then(Value::as_str).is_some_and(|s| s != "success");
            let text = event.get("result").and_then(Value::as_str).unwrap_or_default().to_string();
            seen.result = Some(Answer { ok: !failed, text });
        }
        _ => {}
    }
    seen
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run ids with a supervisor thread in this process.
static LIVE: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// Engine processes going now, against "runs at once".
static ACTIVE: Mutex<usize> = Mutex::new(0);

/// Whether this process is supervising run `id`.
pub fn is_live(id: &str) -> bool {
    guard(&LIVE).contains(id)
}

struct Live(String);

impl Live {
    fn register(id: &str) -> Live {
        guard(&LIVE).insert(id.to_string());
        Live(id.to_string())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        guard(&LIVE).remove(&self.0);
    }
}

struct Slot;

impl Slot {
    fn take(limit: u32) -> Option<Slot> {
        let mut active = guard(&ACTIVE);
        if *active < usize::try_from(limit).unwrap_or(usize::MAX) {
            *active += 1;
            Some(Slot)
        } else {
            None
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut active = guard(&ACTIVE);
        *active = active.saturating_sub(1);
    }
}

/// A pid is looked at only when it names one process: never 0 or 1, and never one that wraps to a negative number, which
/// `kill` reads as a process group or as everything.
fn single_pid(pid: u32) -> bool {
    pid >= 2 && i32::try_from(pid).is_ok()
}

/// When process `pid` started, as `ps -o lstart=` prints it (`None` when there is no such process). Two processes that
/// had the same pid at different times have different start times.
pub fn process_started(pid: u32) -> Option<String> {
    if !single_pid(pid) {
        return None;
    }
    let out = Command::new("ps").env("LC_ALL", "C").args(["-o", "lstart=", "-p", &pid.to_string()]).stderr(Stdio::null()).output().ok()?;
    let started = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !started.is_empty()).then_some(started)
}

/// Whether `pid` is the process that started at `start`. Without a recorded start time nothing is verified.
pub fn is_same_process(pid: u32, start: Option<&str>) -> bool {
    start.is_some_and(|start| process_started(pid).as_deref() == Some(start))
}

/// Whether the process that supervises a run is still going. A pid that now belongs to another process is not it.
pub fn owner_alive(pid: u32, start: Option<&str>) -> bool {
    match start {
        Some(start) => is_same_process(pid, Some(start)),
        None => process_started(pid).is_some(),
    }
}

/// Ends process `pid` now, only if it is verified to be the one that started at `start`. A reused pid is left alone.
pub fn kill_verified(pid: u32, start: Option<&str>) -> bool {
    is_same_process(pid, start)
        && Command::new("kill").arg("-KILL").arg(pid.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Everything a run needs once it is created.
pub struct Job {
    /// The data directory.
    pub dir: PathBuf,
    /// The engine program: `claude`, or whatever `PR_CRAFT_CLAUDE` names.
    pub program: PathBuf,
    /// The run as first saved, queued.
    pub run: Run,
    pub prompt: String,
    /// The diff the answer is checked against.
    pub files: Vec<FileDiff>,
    pub settings: EngineSettings,
}

/// Saves the run, then supervises it on its own thread. A short-lived process has to wait for the handle, or the run ends
/// with it. The run counts as live before it is saved, so `run.list` never mistakes a run being started for an orphan.
pub fn launch(job: Job) -> Result<JoinHandle<()>, String> {
    let id = job.run.id.clone();
    let live = Live::register(&id);
    store::create_run(&job.dir, &job.run)?;
    thread::Builder::new()
        .name(format!("ai-run-{id}"))
        .spawn(move || {
            let _live = live;
            supervise(job);
        })
        .map_err(|error| format!("could not start the thread for run {id}: {error}"))
}

/// Copies what the supervisor owns from its working copy onto the run as it is on disk now. What other writers own
/// (`drafts[].state` through `draft.keep`, `stopping` through `run.stop`) is kept from the disk copy. A run that already
/// ended (settled as orphaned by someone else) is left as it is.
pub(crate) fn merge_supervised(disk: &mut Run, local: &Run) {
    if !disk.state.is_active() {
        return;
    }
    disk.state = local.state;
    disk.started = local.started;
    disk.ended = local.ended;
    disk.last_output = local.last_output;
    disk.pid = local.pid;
    disk.pid_start.clone_from(&local.pid_start);
    disk.files.clone_from(&local.files);
    disk.skipped.clone_from(&local.skipped);
    disk.redactions = local.redactions;
    disk.tree.clone_from(&local.tree);
    disk.dropped = local.dropped;
    disk.long_step = local.long_step;
    disk.retries = local.retries;
    disk.error.clone_from(&local.error);
    disk.tail.clone_from(&local.tail);
    disk.stopping |= local.stopping;
    if disk.drafts.is_empty() {
        disk.drafts.clone_from(&local.drafts);
    }
}

/// Saves the run through [`store::with_run`], at most once a second while output streams in, and at once when forced.
struct Saver<'a> {
    dir: &'a Path,
    last: Instant,
}

impl Saver<'_> {
    fn save(&mut self, run: &Run, force: bool) {
        if !force && self.last.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last = Instant::now();
        if let Err(error) = store::with_run(self.dir, &run.id, |disk| merge_supervised(disk, run)) {
            eprintln!("pr-craft: could not save run {}: {error}", run.id);
        }
    }
}

enum Ended {
    Answer(Answer),
    Killed,
    /// Stop: the text of the last assistant message that finished before it, if any.
    Stopped(Option<String>),
    Failed(String),
    /// Quiet too long and allowed one retry: the process was killed to start it again.
    Stuck,
}

const POLL: Duration = Duration::from_millis(100);

/// How long a process may linger after it printed its result, or was told to end, before it is killed.
const LINGER: Duration = Duration::from_secs(2);

fn supervise(job: Job) {
    let Job { dir, program, mut run, prompt, files, settings } = job;
    let mut saver = Saver { dir: &dir, last: Instant::now() };
    let Some(_slot) = wait_for_slot(&dir, &mut run, &settings, &mut saver) else { return };
    begin(&mut run);
    saver.save(&run, true);
    let ended = loop {
        match attempt(&dir, &program, &prompt, &mut run, &settings, &mut saver) {
            Ended::Stuck => {
                run.retries += 1;
                run.push_tail("No output for too long: ended it and started it again once.".into());
                run.pid = None;
                run.pid_start = None;
                begin(&mut run);
                saver.save(&run, true);
            }
            other => break other,
        }
    };
    finish(&mut run, ended, &files);
    store::clear_signal(&dir, &run.id);
    saver.save(&run, true);
}

/// Waits for a free slot. `None` when Stop or Kill came first; the run is then already ended and saved.
fn wait_for_slot(dir: &Path, run: &mut Run, settings: &EngineSettings, saver: &mut Saver) -> Option<Slot> {
    loop {
        if let Some(signal) = store::read_signal(dir, &run.id) {
            finish(run, if signal == Signal::Kill { Ended::Killed } else { Ended::Stopped(None) }, &[]);
            store::clear_signal(dir, &run.id);
            saver.save(run, true);
            return None;
        }
        if let Some(slot) = Slot::take(settings.runs_at_once) {
            return Some(slot);
        }
        thread::sleep(POLL);
    }
}

fn begin(run: &mut Run) {
    let now = Utc::now();
    run.state = RunState::Running;
    run.started = now;
    run.last_output = None;
    run.long_step = false;
    if let Some(root) = run.tree.first_mut() {
        root.status = NodeStatus::Running;
        root.started = Some(now);
        root.files_read.clear();
    }
}

fn attempt(dir: &Path, program: &Path, prompt: &str, run: &mut Run, settings: &EngineSettings, saver: &mut Saver) -> Ended {
    let sandbox = dir.join("sandbox").join(&run.id);
    if let Err(error) = fs::create_dir_all(&sandbox) {
        return Ended::Failed(format!("could not make the engine's empty working folder {}: {error}", sandbox.display()));
    }
    let ended = drive(&sandbox, program, prompt, run, settings, saver);
    // Best effort: an empty folder left behind is harmless.
    let _ = fs::remove_dir_all(&sandbox);
    ended
}

fn background(work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    thread::Builder::new().spawn(work).map(drop).map_err(|error| format!("could not start a reader thread: {error}"))
}

/// Starts the engine in `sandbox`, an empty folder, and follows it until it ends. The run's state and tree follow the output.
///
/// What Stop means: with no tools the engine has one step, writing its answer. Stop ends the process at once. The run ends
/// `stopped`; an assistant message that had already finished is kept as findings, a message still being written is not.
fn drive(sandbox: &Path, program: &Path, prompt: &str, run: &mut Run, settings: &EngineSettings, saver: &mut Saver) -> Ended {
    let spawned =
        Command::new(program).args(CLAUDE_ARGS).current_dir(sandbox).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return Ended::Failed(format!(
                "could not start `{}`: {error}. Install Claude Code, or set PR_CRAFT_CLAUDE to its path",
                program.display()
            ));
        }
    };
    run.pid = Some(child.id());
    run.pid_start = process_started(child.id());
    saver.save(run, true);

    let (lines, incoming) = mpsc::channel::<String>();
    let (stderr_sink, stderr_text) = mpsc::channel::<String>();
    let readers = (|| {
        if let Some(mut stdin) = child.stdin.take() {
            let prompt = prompt.to_string();
            // An engine that stops reading ends the run by exiting; the write error says nothing more.
            background(move || drop(stdin.write_all(prompt.as_bytes())))?;
        }
        if let Some(stdout) = child.stdout.take() {
            background(move || {
                let mut reader = BufReader::new(stdout);
                let mut buffer = Vec::new();
                while reader.read_until(b'\n', &mut buffer).is_ok_and(|read| read > 0) {
                    let line = String::from_utf8_lossy(&buffer).trim_end().to_string();
                    buffer.clear();
                    if lines.send(line).is_err() {
                        break;
                    }
                }
            })?;
        }
        if let Some(stderr) = child.stderr.take() {
            background(move || {
                let mut bytes = Vec::new();
                drop(stderr.take(64 * 1024).read_to_end(&mut bytes));
                // The receiver is gone when the run ended some other way; nobody wants the text then.
                drop(stderr_sink.send(String::from_utf8_lossy(&bytes).into_owned()));
            })?;
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = readers {
        drop(child.kill());
        drop(child.wait());
        return Ended::Failed(error);
    }

    let can_retry = settings.stuck.retry_once && run.retries == 0;
    let (mut answer, mut last_text) = (None, None);
    let (mut killed, mut retry, mut stopped) = (false, false, false);
    let mut ending: Option<Instant> = None;
    loop {
        match incoming.recv_timeout(POLL) {
            Ok(line) => {
                let seen = parse_event(&line);
                if seen.assistant_text.is_some() && !stopped {
                    last_text.clone_from(&seen.assistant_text);
                }
                note(run, seen, &mut answer, !stopped);
                if answer.is_some() {
                    ending.get_or_insert_with(Instant::now);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        match store::read_signal(saver.dir, &run.id) {
            Some(Signal::Kill) if !killed => {
                killed = true;
                ending.get_or_insert_with(Instant::now);
                drop(child.kill());
            }
            Some(Signal::Stop) if !stopped && !killed && answer.is_none() => {
                stopped = true;
                run.stopping = true;
                ending.get_or_insert_with(Instant::now);
                drop(child.kill());
                saver.save(run, true);
            }
            _ => {}
        }
        if ending.is_none() && stuck(run, Utc::now(), &settings.stuck) {
            if can_retry {
                retry = true;
                ending = Some(Instant::now());
                drop(child.kill());
            } else if run.state == RunState::Running {
                run.state = RunState::LooksStuck;
                saver.save(run, true);
            }
        }
        // A process that printed its result and did not exit, or a grandchild that kept the pipe open, must not hold the run.
        if ending.is_some_and(|at| at.elapsed() > LINGER) {
            break;
        }
        saver.save(run, false);
    }
    drop(child.kill());
    let status = child.wait();
    // A kill sent from another process may end the child before this loop reads its marker; a finished answer still counts.
    if answer.is_none() && store::read_signal(saver.dir, &run.id) == Some(Signal::Kill) {
        killed = true;
    }
    if killed && answer.is_none() {
        Ended::Killed
    } else if let Some(answer) = answer {
        Ended::Answer(answer)
    } else if stopped {
        Ended::Stopped(last_text)
    } else if retry {
        Ended::Stuck
    } else {
        // The reader ends when the pipe closes, which is when the engine exits; a grandchild holding it open is waited for briefly.
        let stderr = stderr_text.recv_timeout(Duration::from_secs(1)).unwrap_or_default().trim().to_string();
        let status = status.map_or_else(|error| format!("could not read its exit status: {error}"), |status| status.to_string());
        let said = if stderr.is_empty() { String::new() } else { format!(": {}", clip(&stderr, 400)) };
        Ended::Failed(format!("{} ended ({status}) without an answer{said}", program.display()))
    }
}

/// Applies one line of output to the run: its clock and log. A result is taken as the answer only while `accept` is set.
fn note(run: &mut Run, seen: Seen, answer: &mut Option<Answer>, accept: bool) {
    run.last_output = Some(Utc::now());
    if run.state == RunState::LooksStuck {
        run.state = RunState::Running;
    }
    if let Some(summary) = seen.summary {
        run.push_tail(summary);
    }
    if accept && seen.result.is_some() {
        *answer = seen.result;
    }
}

fn drafts_of(run: &mut Run, text: &str, files: &[FileDiff]) -> bool {
    let Some((found, dropped)) = parse_findings(text, files, &run.agent.name) else { return false };
    run.drafts = found
        .into_iter()
        .enumerate()
        .map(|(number, finding)| AiDraft { id: format!("{}/{number}", run.id), finding, state: DraftState::Pending })
        .collect();
    run.dropped = dropped;
    true
}

/// Writes the outcome into the run: its state, its error or drafts, and the root node.
fn finish(run: &mut Run, ended: Ended, files: &[FileDiff]) {
    let now = Utc::now();
    let (state, status, error) = match ended {
        Ended::Answer(answer) if answer.ok => {
            if drafts_of(run, &answer.text, files) {
                (RunState::Done, NodeStatus::Done, None)
            } else {
                let said = format!("the engine's answer held no findings list, so nothing was added. It said: {}", clip(&answer.text, 300));
                (RunState::Failed, NodeStatus::Failed, Some(said))
            }
        }
        Ended::Answer(answer) => {
            let said = if answer.text.trim().is_empty() { "the engine reported an error without saying what".to_string() } else { answer.text };
            (RunState::Failed, NodeStatus::Failed, Some(said))
        }
        Ended::Killed => (RunState::Killed, NodeStatus::Stopped, None),
        Ended::Stopped(text) => {
            if let Some(text) = text {
                drafts_of(run, &text, files);
            }
            (RunState::Stopped, NodeStatus::Stopped, None)
        }
        Ended::Failed(message) => (RunState::Failed, NodeStatus::Failed, Some(message)),
        Ended::Stuck => (RunState::Failed, NodeStatus::Failed, Some("it was quiet again after the retry".to_string())),
    };
    run.state = state;
    run.error = error;
    run.ended = Some(now);
    run.long_step = false;
    let findings = u32::try_from(run.drafts.len()).unwrap_or(u32::MAX);
    if let Some(root) = run.tree.first_mut() {
        root.status = status;
        root.ended = Some(now);
        root.findings = findings;
    }
}

#[cfg(test)]
mod tests {
    use pr_craft_core::diff::parse_unified;
    use pr_craft_core::redact::MASK;

    use super::*;

    const DIFF: &str = "diff --git a/a.go b/a.go
index 1111111..2222222 100644
--- a/a.go
+++ b/a.go
@@ -1,4 +1,4 @@
 package a
-var old = 1
+var key = \"ghp_0123456789abcdef0123456789abcdef0123\"
 func g() {}
diff --git a/deploy/.env b/deploy/.env
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/deploy/.env
@@ -0,0 +1 @@
+DB_HOST=internal-db-9
";

    #[test]
    fn the_prompt_holds_neither_a_never_send_file_nor_a_secret() {
        let files = parse_unified(DIFF);
        let rules = ReadRules::default();
        let prompt = build_prompt(&PromptInput {
            template: "Review {pr_title} against {base_branch}. Files: {files}",
            reference: "acme/api #482",
            title: "Retry webhooks",
            base_branch: "main",
            files: &files,
            rules: &rules,
        });
        assert_eq!(prompt.sent, vec!["a.go"]);
        assert_eq!(prompt.skipped.len(), 1, "the .env file is named as skipped");
        assert!(!prompt.text.contains("internal-db-9") && !prompt.text.contains("deploy/.env"), "never-send files are dropped whole");
        assert!(!prompt.text.contains("ghp_0123"), "the token is masked");
        assert!(prompt.text.contains(MASK));
        assert_eq!(prompt.redactions, 1);
        assert!(prompt.text.contains("Review Retry webhooks against main. Files: a.go"), "variables are filled in");
        assert!(prompt.text.contains("=== a.go") && prompt.text.contains("-O2| var old = 1"), "the diff follows, each line tagged");
        assert!(prompt.text.contains("+L2| var key") && prompt.text.contains(" L1| package a"), "added and context lines are tagged by the new file");
        let loose = ReadRules { never_send: Vec::new(), redact_secrets: false, ..ReadRules::default() };
        let all = build_prompt(&PromptInput { template: "x", reference: "r", title: "t", base_branch: "b", files: &files, rules: &loose });
        assert!(all.text.contains("ghp_0123") && all.text.contains("internal-db-9"), "the rules are what held them back");
    }

    #[test]
    fn a_diff_placeholder_in_the_prompt_takes_the_diff_where_it_stands() {
        let files = parse_unified(DIFF);
        let rules = ReadRules::default();
        let at = |template| build_prompt(&PromptInput { template, reference: "r", title: "t", base_branch: "b", files: &files, rules: &rules }).text;
        let placed = at("Look here:\n{diff}\nThen answer.");
        assert_eq!(placed.matches("=== a.go").count(), 1, "the diff is in the text once");
        assert!(placed.find("=== a.go") < placed.find("Then answer."));
    }

    #[test]
    fn a_finding_lands_on_the_line_its_tag_names() {
        let files = parse_unified(DIFF);
        let answer = r#"Here you go:
```json
[
  {"path": "a.go", "line": "L2", "body": "  Hard-coded token.  "},
  {"path": "a.go", "line": "O2", "body": "Removed default mattered."},
  {"path": "a.go", "line": "L1", "body": "On a context line."},
  {"path": "a.go", "line": "L99", "body": "Not in the diff."},
  {"path": "a.go", "line": "O1", "body": "Line 1 was not removed."},
  {"path": "gone.go", "line": "L1", "body": "Not a file of the run."},
  {"path": "a.go", "line": "L1", "body": "   "},
  {"path": "a.go", "body": "No line."}
]
```"#;
        let (found, dropped) = parse_findings(answer, &files, "Go idioms").expect("an array");
        assert_eq!(dropped, 5);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].body, "Hard-coded token.");
        assert_eq!(found[0].line, LineRef { old: None, new: Some(2) }, "an added line has only the new side");
        assert_eq!(found[1].line, LineRef { old: Some(2), new: None }, "O2 is the removed line, not the added line that is also 2");
        assert_eq!(found[2].line, LineRef { old: Some(1), new: Some(1) });
        assert!(found.iter().all(|finding| finding.agent == "Go idioms"));
    }

    #[test]
    fn a_bare_number_that_two_lines_carry_is_dropped_not_guessed() {
        let files = parse_unified(DIFF);
        let one = |line: &str| parse_findings(&format!(r#"[{{"path":"a.go","line":{line},"body":"b"}}]"#), &files, "x").expect("array");
        assert_eq!(one("2"), (Vec::new(), 1), "line 2 is both the removed and the added line");
        assert_eq!(one("3").0.len(), 1, "a number only one line carries is unambiguous");
        let old = parse_findings(r#"[{"path":"a.go","line":2,"side":"old","body":"b"}]"#, &files, "x").expect("array");
        assert_eq!(old.0[0].line, LineRef { old: Some(2), new: None }, "a side says which");
    }

    #[test]
    fn an_answer_without_a_findings_list_is_none_and_an_empty_list_is_no_findings() {
        let files = parse_unified(DIFF);
        assert!(parse_findings("Looks good to me [really].", &files, "x").is_none(), "prose is not an empty review");
        assert!(parse_findings("I found 3 things: [1, 2, 3]", &files, "x").is_none(), "an array of numbers is not findings");
        assert_eq!(parse_findings("[]", &files, "x"), Some((Vec::new(), 0)));
    }

    #[test]
    fn a_stream_line_tells_the_text_and_the_result() {
        let said = parse_event(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"\n[{\"a\":1}]"}]}}"#);
        assert_eq!(said.assistant_text.as_deref(), Some("\n[{\"a\":1}]"));
        assert!(said.summary.is_some());
        let failed = parse_event(r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"usage limit"}"#);
        assert!(failed.result.is_some_and(|a| !a.ok));
        assert!(parse_event(r#"{"type":"stream_event","event":{}}"#).result.is_none());
        assert!(parse_event("not json at all").summary.is_some(), "a plain line still counts as output");
    }

    #[test]
    fn a_pid_is_only_ever_signalled_while_it_is_the_process_we_started() {
        for pid in [0, 1, u32::MAX, 0x8000_0000] {
            assert!(!kill_verified(pid, Some("anything")), "{pid}");
            assert!(process_started(pid).is_none(), "{pid}");
        }
        let mut other = Command::new("sleep").arg("30").spawn().expect("a bystander process");
        let pid = other.id();
        let start = process_started(pid).expect("it is running");
        assert!(!kill_verified(pid, None), "no recorded start time: never signalled");
        assert!(!kill_verified(pid, Some("Thu Jan  1 00:00:00 1970")), "a reused pid has another start time");
        assert!(process_started(pid).is_some(), "the bystander is still running");
        assert!(owner_alive(pid, Some(&start)) && !owner_alive(pid, Some("Thu Jan  1 00:00:00 1970")));
        assert!(kill_verified(pid, Some(&start)), "the verified process is ended");
        drop(other.wait());
    }
}
