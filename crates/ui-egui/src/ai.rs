//! The UI's side of the AI engine: typed calls over the engine's commands, background jobs, the run poller and the
//! pure models the screens draw from (slash menu, coverage grid, run filters, the start gate, the agent form).

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use craft::egui::Context;
use pr_craft_core::agent::{Agent, Engine, RepoRef, RunsOn, SpawnLimits, Trigger};
use pr_craft_core::diff::DiffLine;
use pr_craft_core::review::Anchor;
use pr_craft_core::run::{AiDraft, DraftState, Finding, Run, RunState};
use pr_craft_core::{Change, ChangeRef, Forge};
use pr_craft_engine::ai::{EngineView, Kept, Plan, Preview, ReviewPreview, RunList};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// How often the run list is read while a run is going, and while none is.
const POLL_ACTIVE: Duration = Duration::from_secs(2);
const POLL_IDLE: Duration = Duration::from_secs(6);
/// Runs that ended up to this long before the oldest active run of a change still belong to the same review.
const BATCH_SLACK_SECS: i64 = 120;

// ---- background jobs ----

/// One background lookup the screen shows when it lands.
pub(crate) enum Fetch<T> {
    Idle,
    Loading(Receiver<Result<T, String>>),
    Ready(T),
    Failed(String),
}

impl<T: Send + 'static> Fetch<T> {
    /// Runs `job` on its own thread: the CLIs wait on the network.
    pub(crate) fn start(ctx: &Context, job: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        Fetch::Loading(spawn(ctx, job))
    }

    pub(crate) fn poll(&mut self) {
        let Fetch::Loading(rx) = self else { return };
        *self = match rx.try_recv() {
            Ok(Ok(value)) => Fetch::Ready(value),
            Ok(Err(error)) => Fetch::Failed(error),
            Err(TryRecvError::Disconnected) => Fetch::Failed("the lookup stopped before answering".into()),
            Err(TryRecvError::Empty) => return,
        };
    }

    pub(crate) fn loading(&self) -> bool {
        matches!(self, Fetch::Loading(_))
    }
}

fn spawn<T: Send + 'static>(ctx: &Context, job: impl FnOnce() -> Result<T, String> + Send + 'static) -> Receiver<Result<T, String>> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // The screen may have closed first; nobody is left to tell.
        let _ = tx.send(job());
        ctx.request_repaint();
    });
    rx
}

// ---- typed calls ----

fn call<T: DeserializeOwned>(id: &str, params: &Value) -> Result<T, String> {
    let value = pr_craft_engine::run(id, params).map_err(crate::fail)?;
    serde_json::from_value(value).map_err(|e| format!("{id} answered something unreadable: {e}"))
}

pub(crate) fn engine_get() -> Result<EngineView, String> {
    call("ai.engine.get", &Value::Null)
}

pub(crate) fn engine_set(patch: &Value) -> Result<EngineView, String> {
    call("ai.engine.set", patch)
}

pub(crate) fn read_set(forge: Forge, repo: &str, rules: Option<&pr_craft_core::redact::ReadRules>) -> Result<EngineView, String> {
    call("ai.read.set", &json!({ "forge": forge, "repo": repo, "rules": rules }))
}

pub(crate) fn agents() -> Result<Vec<Agent>, String> {
    call("agent.list", &Value::Null)
}
/// The agents that run on this change's repository, ticked in AI review or not.
pub(crate) fn agents_on(change: &ChangeRef) -> Result<Vec<Agent>, String> {
    call("agent.list", &json!({ "forge": change.forge, "repo": change.repo }))
}

pub(crate) fn agent_save(id: Option<&str>, form: &AgentForm) -> Result<Agent, String> {
    let mut params = form.params();
    match id {
        Some(id) => {
            params["id"] = json!(id);
            call("agent.update", &params)
        }
        None => call("agent.create", &params),
    }
}

pub(crate) fn agent_delete(id: &str) -> Result<(), String> {
    call::<Value>("agent.delete", &json!({ "id": id })).map(drop)
}

/// Where an agent runs, changed on its own: Settings → Repositories.
pub(crate) fn agent_set_runs_on(id: &str, runs_on: &RunsOn) -> Result<Agent, String> {
    call("agent.update", &json!({ "id": id, "runs_on": runs_on }))
}

/// J and K in a list: the row after or before `selected`, the first when none is selected.
pub(crate) fn nav_step(selected: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match (selected, forward) {
        (None, _) => 0,
        (Some(i), true) => (i + 1).min(last),
        (Some(i), false) => i.saturating_sub(1).min(last),
    })
}

pub(crate) fn scope_preview(agent: &str) -> Result<Preview, String> {
    call("agent.scope_preview", &json!({ "agent": agent }))
}

pub(crate) fn plan(change: &ChangeRef, options: &ReviewOptions) -> Result<Plan, String> {
    call("ai.review.plan", &json!({ "change": change, "options": options.json(None) }))
}

pub(crate) fn preview(change: &ChangeRef, options: &ReviewOptions) -> Result<ReviewPreview, String> {
    call("ai.review.preview", &json!({ "change": change, "options": options.json(None) }))
}

/// `ai.review.start` without waiting: the desktop app never blocks on a review. Returns the run ids.
pub(crate) fn start(change: &ChangeRef, options: &ReviewOptions, expect_hashes: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let mut params = json!({ "change": change, "options": options.json(Some(false)) });
    if let Some(hashes) = expect_hashes {
        params["expect_hashes"] = json!(hashes);
    }
    let started: Value = call("ai.review.start", &params)?;
    Ok(started["runs"].as_array().map(|ids| ids.iter().filter_map(|id| id.as_str().map(String::from)).collect()).unwrap_or_default())
}

pub(crate) fn runs() -> Result<RunList, String> {
    call("run.list", &Value::Null)
}

pub(crate) fn run_stop(run: &str) -> Result<(), String> {
    call::<Value>("run.stop", &json!({ "run": run })).map(drop)
}

pub(crate) fn run_kill(run: &str) -> Result<(), String> {
    call::<Value>("run.kill", &json!({ "run": run })).map(drop)
}

pub(crate) fn stop_all() -> Result<Vec<String>, String> {
    let stopped: Value = call("run.stop_all", &Value::Null)?;
    Ok(stopped["stopped"].as_array().map(|ids| ids.iter().filter_map(|id| id.as_str().map(String::from)).collect()).unwrap_or_default())
}

pub(crate) fn keep(change: &ChangeRef, draft: &str) -> Result<Kept, String> {
    call("draft.keep", &json!({ "change": change, "draft": draft }))
}

pub(crate) fn discard(change: &ChangeRef, draft: &str) -> Result<(), String> {
    call::<Value>("draft.discard", &json!({ "change": change, "draft": draft })).map(drop)
}

/// Quit: ends every run, then waits a moment so the engine processes are gone before this process is.
/// A run left behind is failed, and its verified engine killed, by the next `run.list`.
pub(crate) fn stop_everything_on_quit() {
    if !runs().is_ok_and(|list| list.runs.iter().any(|run| run.state.is_active())) {
        return;
    }
    // Best effort: a failure here leaves the next start to settle the runs.
    let _ = stop_all();
    for _ in 0..20 {
        if !runs().is_ok_and(|list| list.runs.iter().any(|run| run.state.is_active())) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---- the review options ----

/// What the AI review sheet offers: the options `ai.review.*` take. Merging duplicates is not built.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ReviewOptions {
    pub skip_viewed: bool,
    /// `None`: every agent that runs on the repository.
    pub agents: Option<Vec<String>>,
    /// Leave out the General run: the chosen agents read their files and nothing else.
    pub no_general: bool,
}

impl ReviewOptions {
    fn json(&self, wait: Option<bool>) -> Value {
        let mut options = json!({ "skip_viewed": self.skip_viewed });
        if let Some(agents) = &self.agents {
            options["agents"] = json!(agents);
        }
        if self.no_general {
            options["general"] = json!(false);
        }
        if let Some(wait) = wait {
            options["wait"] = json!(wait);
        }
        options
    }
}

/// How a job ended: what to say, and whether it only went partway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Done {
    pub text: String,
    pub warn: bool,
}

impl Done {
    pub(crate) fn ok(text: String) -> Self {
        Done { text, warn: false }
    }
}

/// What a paid start says when it ends: success, failure when nothing started, and a warning when it went partway.
pub(crate) fn started_summary(agent: &str, started: usize, errors: &[String], activity_key: &str) -> Result<Done, String> {
    match (started, errors.is_empty()) {
        (_, true) => Ok(Done::ok(format!("Started {started} runs of {agent}. They appear in Activity ({activity_key})."))),
        (0, false) => Err(errors.join(" ")),
        (_, false) => Ok(Done { text: format!("Started {started} runs, but not all. Not started: {}", errors.join(" ")), warn: true }),
    }
}

/// A start that spends engine usage, waiting for the reviewer's yes. Nothing runs until `run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaidStart {
    pub agent: String,
    pub engine: Engine,
    /// Each change, and the files the agent read last time or would read (for the reviewer to see).
    pub targets: Vec<(ChangeRef, Vec<String>)>,
    pub options: ReviewOptions,
    /// The General review: it covers every readable file of the change, not only the files of the earlier run.
    pub general: bool,
    /// A run that looks stuck, ended first (Kill and retry).
    pub kill: Option<String>,
    /// The files listed are what the last run read, not a fresh plan.
    pub from_last_run: bool,
}

impl PaidStart {
    /// Retry of one run: its own agent only, or the General review for a general run.
    /// `engine` is the one the start will use now: the agent's current engine, or the default for the General review.
    pub(crate) fn retry(run: &Run, engine: Engine) -> Self {
        let general = run.agent.id.is_none();
        let options = match &run.agent.id {
            Some(id) => ReviewOptions { skip_viewed: false, agents: Some(vec![id.clone()]), no_general: true },
            None => ReviewOptions { skip_viewed: false, agents: Some(Vec::new()), no_general: false },
        };
        PaidStart {
            agent: run.agent.name.clone(),
            engine,
            targets: vec![(run.change.clone(), run.files.clone())],
            options,
            general,
            kill: run.state.is_active().then(|| run.id.clone()),
            from_last_run: true,
        }
    }

    /// One agent on the changes its scope matches (frame 30, "Run on these N now"): no other agent, no General review.
    pub(crate) fn agent_on(agent: &Agent, targets: Vec<(ChangeRef, Vec<String>)>) -> Self {
        let options = ReviewOptions { skip_viewed: false, agents: Some(vec![agent.id.clone()]), no_general: true };
        PaidStart { agent: agent.name.clone(), engine: agent.engine, targets, options, general: false, kill: None, from_last_run: false }
    }

    /// What the confirm step says: who, where, which files, what is not covered, and that it spends usage.
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!("Start {} on {}:", self.agent, self.engine.name())];
        for (change, files) in &self.targets {
            let shown: Vec<&str> = files.iter().take(5).map(String::as_str).collect();
            let more = if files.len() > shown.len() { format!(" and {} more", files.len() - shown.len()) } else { String::new() };
            let known = if self.from_last_run { "the last run read" } else { "matches" };
            lines.push(format!("{}: {known} {} files ({}{more})", change.reference(), files.len(), shown.join(", ")));
        }
        lines.push(if self.general {
            "This is the General review: it reads every readable file of the change as it is now, not only the files the last run read.".into()
        } else {
            "Only this agent's files are read, as the change is now: the list above may differ. No General review and no other agent runs.".into()
        });
        if self.kill.is_some() {
            lines.push("The run that looks stuck is ended first.".into());
        }
        lines.push(if self.engine.wired() {
            format!("This spends {} usage, and the run starts at once.", self.engine.name())
        } else {
            format!("{} is not wired yet, so the run fails at once and costs nothing.", self.engine.name())
        });
        lines
    }

    /// Ends the stuck run if asked, then starts on each change, one after another. Blocks: call it off the UI thread.
    pub(crate) fn run(self, activity_key: String) -> Result<Done, String> {
        if let Some(id) = &self.kill {
            run_kill(id)?;
        }
        let (mut started, mut errors) = (0, Vec::new());
        for (change, _) in &self.targets {
            match start(change, &self.options, None) {
                Ok(ids) => started += ids.len(),
                Err(error) => errors.push(format!("{}: {error}", change.reference())),
            }
        }
        started_summary(&self.agent, started, &errors, &activity_key)
    }
}

/// The preview the reviewer saw, and the options it was made with. Start is allowed only on a gate that still matches
/// the sheet, and sends the hashes of every prompt in it, so a change that moved since is refused by the engine.
pub(crate) struct StartGate {
    options: ReviewOptions,
    hashes: Vec<String>,
}

impl StartGate {
    pub(crate) fn of(preview: &ReviewPreview, options: &ReviewOptions) -> Self {
        StartGate { options: options.clone(), hashes: preview.prompts.iter().map(|p| p.prompt_hash.clone()).collect() }
    }

    /// Whether what the reviewer saw is what the sheet would start now.
    pub(crate) fn matches(&self, options: &ReviewOptions) -> bool {
        self.options == *options
    }

    pub(crate) fn hashes(&self) -> Vec<String> {
        self.hashes.clone()
    }
}

// ---- the coverage grid ----

/// The files × agents map of the sheet (frames 29 and 54): one column per planned agent, then the general pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Grid {
    pub columns: Vec<String>,
    /// A file, and for each column whether it reads it; the last column is the general pass.
    pub rows: Vec<(String, Vec<bool>)>,
}

impl Grid {
    pub(crate) fn of(plan: &Plan) -> Self {
        let mut columns: Vec<String> = plan.agents.iter().map(|a| a.name.clone()).collect();
        columns.push("General".into());
        let rows = plan
            .coverage
            .files
            .iter()
            .map(|path| {
                let covered = plan.coverage.cells.get(path).map(Vec::as_slice).unwrap_or_default();
                let mut row: Vec<bool> = plan.agents.iter().map(|a| covered.contains(&a.id)).collect();
                row.push(plan.coverage.general.contains(path));
                (path.clone(), row)
            })
            .collect();
        Grid { columns, rows }
    }

    /// Files that no column reads: none when the grid is sound.
    pub(crate) fn uncovered(&self) -> usize {
        self.rows.iter().filter(|(_, row)| !row.iter().any(|&read| read)).count()
    }
}

/// How many runs a start would make: one per agent with files, and one general run when files are left over.
pub(crate) fn run_count(plan: &Plan) -> usize {
    plan.agents.iter().filter(|a| !a.files.is_empty()).count() + usize::from(!plan.general.is_empty())
}

// ---- runs ----

/// Test seam: the engine a retried run would use, found without reading this Mac's agents.
#[cfg(test)]
pub(crate) type EngineLookup = fn(&Run) -> Result<Engine, String>;

/// The runs of every change, read on a timer off the UI thread, and the actions started on them.
#[derive(Default)]
pub(crate) struct Runs {
    runs: Vec<Run>,
    unreadable: Vec<String>,
    pub error: Option<String>,
    inflight: Option<Receiver<Result<RunList, String>>>,
    last: Option<Instant>,
    refresh: bool,
    acts: Vec<Receiver<Result<Done, String>>>,
    /// The text of a status that is a warning: a start that went only partway. It shows in the warning colour.
    pub warning: Option<String>,
    /// What the last action did, for a live region.
    pub status: Option<Result<String, String>>,
    /// A paid start the reviewer has not yet confirmed.
    pub confirm: Option<PaidStart>,
    /// The confirm window is new: its Start button takes focus on its first frame.
    pub confirm_focus: bool,
    /// Test seam: what the engine of a retried run is, without reading this Mac's agents.
    #[cfg(test)]
    pub engine_for: Option<EngineLookup>,
}

impl Runs {
    /// Collects what the threads answered and starts the next read when it is due. Call once a frame.
    pub(crate) fn poll(&mut self, ctx: &Context) {
        self.collect();
        let wait = if self.active().next().is_some() { POLL_ACTIVE } else { POLL_IDLE };
        let due = self.refresh || self.last.is_none_or(|at| at.elapsed() >= wait);
        if self.inflight.is_none() && due {
            self.refresh = false;
            self.inflight = Some(spawn(ctx, runs));
        }
        // Elapsed times tick while a run goes; an idle app wakes only to look for runs started elsewhere.
        ctx.request_repaint_after(if self.active().next().is_some() { Duration::from_secs(1) } else { wait });
    }

    fn collect(&mut self) {
        if let Some(rx) = &self.inflight {
            let answer = match rx.try_recv() {
                Ok(answer) => Some(answer),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("reading the runs stopped before answering".into())),
            };
            if let Some(answer) = answer {
                self.inflight = None;
                self.last = Some(Instant::now());
                match answer {
                    Ok(list) => {
                        (self.runs, self.unreadable, self.error) = (list.runs, list.unreadable, None);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        let mut finished = Vec::new();
        self.acts.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                finished.push(result);
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        if let Some(last) = finished.pop() {
            self.warning = last.as_ref().ok().filter(|done| done.warn).map(|done| done.text.clone());
            self.status = Some(last.map(|done| done.text));
            self.refresh = true;
        }
    }

    /// Reads the list again on the next frame.
    pub(crate) fn refresh_now(&mut self) {
        self.refresh = true;
    }

    pub(crate) fn act(&mut self, ctx: &Context, job: impl FnOnce() -> Result<String, String> + Send + 'static) {
        self.act_done(ctx, move || job().map(Done::ok));
    }

    /// Like `act`, for a job that can finish with a warning.
    pub(crate) fn act_done(&mut self, ctx: &Context, job: impl FnOnce() -> Result<Done, String> + Send + 'static) {
        self.status = None;
        self.acts.push(spawn(ctx, job));
    }

    /// Asks the reviewer to confirm a paid start; the confirm window takes focus when it appears.
    pub(crate) fn ask(&mut self, paid: PaidStart) {
        self.confirm = Some(paid);
        self.confirm_focus = true;
    }

    #[cfg(test)]
    pub(crate) fn with(runs: Vec<Run>) -> Runs {
        Runs { runs, ..Runs::default() }
    }

    pub(crate) fn all(&self) -> &[Run] {
        &self.runs
    }

    pub(crate) fn unreadable(&self) -> &[String] {
        &self.unreadable
    }

    pub(crate) fn of<'a>(&'a self, change: &'a ChangeRef) -> impl Iterator<Item = &'a Run> {
        self.runs.iter().filter(move |run| run.change == *change)
    }

    pub(crate) fn find(&self, id: &str) -> Option<&Run> {
        self.runs.iter().find(|run| run.id == id)
    }

    pub(crate) fn active(&self) -> impl Iterator<Item = &Run> {
        self.runs.iter().filter(|run| run.state.is_active())
    }

    /// Shows a draft's new state at once, before the next read confirms it.
    pub(crate) fn set_draft(&mut self, id: &str, state: DraftState) {
        for draft in self.runs.iter_mut().flat_map(|run| run.drafts.iter_mut()).filter(|d| d.id == id) {
            draft.state = state;
        }
    }
}

/// `7:40`, or `1:02:03` from an hour on.
pub(crate) fn clock(secs: i64) -> String {
    let secs = secs.max(0);
    match (secs / 3600, secs / 60 % 60, secs % 60) {
        (0, m, s) => format!("{m}:{s:02}"),
        (h, m, s) => format!("{h}:{m:02}:{s:02}"),
    }
}

/// How long the run has gone, or went.
pub(crate) fn elapsed_secs(run: &Run, now: DateTime<Utc>) -> i64 {
    run.ended.unwrap_or(now).signed_duration_since(run.started).num_seconds().max(0)
}

/// The state in words: colour is never the only signal.
pub(crate) fn state_word(run: &Run) -> &'static str {
    match run.state {
        _ if run.stopping && run.state.is_active() => "Stopping",
        RunState::Queued => "Queued",
        RunState::Running => "Running",
        RunState::LooksStuck => "Looks stuck",
        RunState::Done => "Done",
        RunState::Failed => "Failed",
        RunState::Stopped => "Stopped",
        RunState::Killed => "Killed",
    }
}

/// The last line the engine printed and how long ago, or that it has printed nothing.
pub(crate) fn last_output(run: &Run, now: DateTime<Utc>) -> String {
    let Some(at) = run.last_output else { return "No output yet".into() };
    let ago = clock(now.signed_duration_since(at).num_seconds());
    match run.tail.last() {
        Some(line) => format!("{line}  ·  {ago} ago"),
        None => format!("Last output {ago} ago"),
    }
}

/// The filters of the Activity window (frame 61).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunFilter {
    Running,
    Stuck,
    Done,
    Failed,
    All,
}

impl RunFilter {
    pub(crate) const ALL: [RunFilter; 5] = [RunFilter::Running, RunFilter::Stuck, RunFilter::Done, RunFilter::Failed, RunFilter::All];

    pub(crate) fn label(self) -> &'static str {
        match self {
            RunFilter::Running => "Running",
            RunFilter::Stuck => "Looks stuck",
            RunFilter::Done => "Done today",
            RunFilter::Failed => "Failed today",
            RunFilter::All => "All",
        }
    }

    pub(crate) fn matches(self, run: &Run, now: DateTime<Utc>) -> bool {
        let today = || run.ended.is_some_and(|at| at.with_timezone(&Local).date_naive() == now.with_timezone(&Local).date_naive());
        match self {
            RunFilter::Running => matches!(run.state, RunState::Queued | RunState::Running),
            RunFilter::Stuck => run.state == RunState::LooksStuck,
            RunFilter::Done => matches!(run.state, RunState::Done | RunState::Stopped) && today(),
            RunFilter::Failed => matches!(run.state, RunState::Failed | RunState::Killed) && today(),
            RunFilter::All => true,
        }
    }
}

/// What the title-area pill says: the change with the newest active run and how far its review is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Pill {
    pub change: ChangeRef,
    pub text: String,
    pub detail: String,
    pub stuck: bool,
}

/// How the runs of one review stand. Failed, stopped and killed runs are not counted as done.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    pub done: usize,
    pub failed: usize,
    pub stopped: usize,
    pub killed: usize,
    /// Queued, running or looks stuck.
    pub going: usize,
    pub stuck: usize,
}

impl Tally {
    pub(crate) fn of<'a>(runs: impl Iterator<Item = &'a Run>) -> Self {
        let mut tally = Tally::default();
        for run in runs {
            match run.state {
                RunState::Done => tally.done += 1,
                RunState::Failed => tally.failed += 1,
                RunState::Stopped => tally.stopped += 1,
                RunState::Killed => tally.killed += 1,
                RunState::LooksStuck => (tally.going, tally.stuck) = (tally.going + 1, tally.stuck + 1),
                RunState::Queued | RunState::Running => tally.going += 1,
            }
        }
        tally
    }

    pub(crate) fn total(&self) -> usize {
        self.done + self.failed + self.stopped + self.killed + self.going
    }

    /// `1 done · 2 failed · 1 going`: the zero counts of failed, stopped, killed and stuck are left out.
    pub(crate) fn summary(&self) -> String {
        let mut parts = vec![format!("{} done", self.done)];
        for (count, word) in [(self.failed, "failed"), (self.stopped, "stopped"), (self.killed, "killed")] {
            if count > 0 {
                parts.push(format!("{count} {word}"));
            }
        }
        parts.push(format!("{} going", self.going));
        if self.stuck > 0 {
            parts.push(format!("{} look stuck", self.stuck));
        }
        parts.join(" · ")
    }
}

pub(crate) fn pill(runs: &[Run]) -> Option<Pill> {
    let newest = runs.iter().find(|run| run.state.is_active())?;
    let change = newest.change.clone();
    let oldest = runs.iter().filter(|r| r.change == change && r.state.is_active()).map(|r| r.started).min()?;
    let floor = oldest - chrono::Duration::seconds(BATCH_SLACK_SECS);
    // Runs of the same review count; an earlier review of the change does not.
    let batch = runs.iter().filter(|r| r.change == change && (r.state.is_active() || r.ended.is_some_and(|at| at >= floor)));
    let tally = Tally::of(batch);
    Some(Pill {
        text: format!("AI review {}  {}/{}", change.reference(), tally.done, tally.total()),
        change,
        detail: tally.summary(),
        stuck: tally.stuck > 0,
    })
}

// ---- AI drafts on the diff ----

/// A finding waiting to be kept or discarded, and whether it was read at the commit now on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub draft: AiDraft,
    /// Its line numbers belong to the diff on screen; otherwise it is listed in the panel only.
    pub placed: bool,
}

/// The pending AI drafts of `change`, ordered by file and line.
pub(crate) fn pending<'a>(runs: impl Iterator<Item = &'a Run>, anchor: &Anchor) -> Vec<Pending> {
    let mut found: Vec<Pending> = runs
        .flat_map(|run| {
            run.drafts.iter().filter(|d| d.state == DraftState::Pending).map(move |d| Pending { draft: d.clone(), placed: run.anchor == *anchor })
        })
        .collect();
    found.sort_by(|a, b| {
        let key = |p: &Pending| (p.draft.finding.path.clone(), p.draft.finding.line.shown());
        key(a).cmp(&key(b))
    });
    found
}

/// Whether `finding` sits on this diff line: on the new side when it has one, else on a removed line's old side.
pub(crate) fn finding_at(finding: &Finding, line: &DiffLine) -> bool {
    match (finding.line.new, finding.line.old) {
        (Some(new), _) => line.new == Some(new),
        (None, Some(old)) => line.new.is_none() && line.old == Some(old),
        (None, None) => false,
    }
}

/// The pending draft a key acts on, as an index into `list`: the one stepped to (⌥↓), else the one under the cursor
/// (`under`, a draft id), else the first draft read at the commit on screen. A draft read at an older commit is never
/// the default: keeping it first would make every current draft fail with "new commits".
pub(crate) fn pick_current(list: &[Pending], focus: Option<&str>, under: Option<&str>) -> Option<usize> {
    let by_id = |id: &str| list.iter().position(|p| p.draft.id == id);
    focus.and_then(by_id).or_else(|| under.and_then(by_id)).or_else(|| list.iter().position(|p| p.placed))
}

/// The first placed pending draft of `path` that sits on one of `lines`.
pub(crate) fn draft_on<'a>(list: &'a [Pending], path: &str, lines: &[DiffLine]) -> Option<&'a Pending> {
    list.iter().find(|p| p.placed && p.draft.finding.path == path && lines.iter().any(|line| finding_at(&p.draft.finding, line)))
}

/// The draft J-like stepping lands on: the one after `from` in `list` order, wrapping, or the first.
pub(crate) fn step_pending(list: &[Pending], from: Option<&str>, forward: bool) -> Option<usize> {
    if list.is_empty() {
        return None;
    }
    let at = from.and_then(|id| list.iter().position(|p| p.draft.id == id));
    Some(match (at, forward) {
        (None, true) => 0,
        (None, false) => list.len() - 1,
        (Some(i), true) => (i + 1) % list.len(),
        (Some(i), false) => (i + list.len() - 1) % list.len(),
    })
}

// ---- the slash menu (frames 28 and 66) ----

/// A variable the prompt may use, offered when the writer types `/`.
pub(crate) struct SlashItem {
    pub label: &'static str,
    pub detail: &'static str,
    pub insert: &'static str,
}

pub(crate) const SLASH_ITEMS: [SlashItem; 4] = [
    SlashItem { label: "/title", detail: "The title of the change", insert: "{pr_title}" },
    SlashItem { label: "/files", detail: "The files in this agent's scope", insert: "{files}" },
    SlashItem { label: "/diff", detail: "The diff of those files", insert: "{diff}" },
    SlashItem { label: "/base", detail: "The base branch", insert: "{base_branch}" },
];

/// The word being typed when the cursor (a character index) sits at its end, if it starts with `/` and the `/` starts
/// the word: `http://x` and `a/b` open nothing. Returns where the word starts and what follows the `/`.
pub(crate) fn slash_query(text: &str, cursor: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let end = cursor.min(chars.len());
    let mut start = end;
    while start > 0 && !chars.get(start - 1).is_none_or(|c| c.is_whitespace()) {
        start -= 1;
    }
    (start < end && chars.get(start) == Some(&'/')).then(|| (start, chars[start + 1..end].iter().collect()))
}

/// The items whose name starts with, or else contains, what was typed after the `/`.
pub(crate) fn slash_matches(query: &str) -> Vec<&'static SlashItem> {
    let query = query.to_lowercase();
    let name = |item: &SlashItem| item.label[1..].to_string();
    let (mut starts, mut contains): (Vec<_>, Vec<_>) =
        SLASH_ITEMS.iter().filter(|i| name(i).contains(&query)).partition(|i| name(i).starts_with(&query));
    starts.append(&mut contains);
    starts
}

/// Replaces the typed `/word` (from `start` to `cursor`) with the item's text. Returns the new cursor.
pub(crate) fn apply_slash(text: &mut String, start: usize, cursor: usize, item: &SlashItem) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = (start.min(chars.len()), cursor.min(chars.len()).max(start.min(chars.len())));
    let mut out: String = chars[..start].iter().collect();
    out.push_str(item.insert);
    out.extend(&chars[end..]);
    *text = out;
    start + item.insert.chars().count()
}

// ---- the agent form ----

/// What the agent editor holds: everything `agent.create` takes, as the fields are typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentForm {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub engine: Engine,
    /// One pattern per line.
    pub include: String,
    /// One pattern per line, without the `!`.
    pub exclude: String,
    pub skills: Vec<String>,
    pub on_request: bool,
    pub spawn: SpawnLimits,
    pub all_repos: bool,
    pub repos: Vec<RepoRef>,
}

impl AgentForm {
    pub(crate) fn new(engine: Engine, spawn: SpawnLimits, repos: Option<RepoRef>) -> Self {
        AgentForm {
            name: String::new(),
            description: String::new(),
            prompt: String::new(),
            engine,
            include: String::new(),
            exclude: String::new(),
            skills: Vec::new(),
            on_request: false,
            spawn,
            all_repos: repos.is_none(),
            repos: repos.into_iter().collect(),
        }
    }

    pub(crate) fn from_agent(agent: &Agent) -> Self {
        let (excluded, included): (Vec<&String>, Vec<&String>) = agent.scope.iter().partition(|p| p.starts_with('!'));
        let lines = |patterns: Vec<&String>, strip: bool| {
            patterns.into_iter().map(|p| if strip { p.trim_start_matches('!') } else { p.as_str() }).collect::<Vec<_>>().join("\n")
        };
        AgentForm {
            name: agent.name.clone(),
            description: agent.description.clone(),
            prompt: agent.prompt.clone(),
            engine: agent.engine,
            include: lines(included, false),
            exclude: lines(excluded, true),
            skills: agent.skills.clone(),
            on_request: agent.triggers.contains(&Trigger::ReviewRequested),
            spawn: agent.spawn,
            all_repos: matches!(agent.runs_on, RunsOn::All),
            repos: match &agent.runs_on {
                RunsOn::All => Vec::new(),
                RunsOn::Repos { repos } => repos.clone(),
            },
        }
    }

    /// The scope as the agent stores it: includes, then excludes with a `!`.
    pub(crate) fn scope(&self) -> Vec<String> {
        let lines = |text: &str| text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect::<Vec<_>>();
        let mut scope = lines(&self.include);
        scope.extend(lines(&self.exclude).into_iter().map(|p| format!("!{}", p.trim_start_matches('!'))));
        scope
    }

    pub(crate) fn runs_on(&self) -> RunsOn {
        if self.all_repos { RunsOn::All } else { RunsOn::Repos { repos: self.repos.clone() } }
    }

    fn params(&self) -> Value {
        let mut triggers = vec![Trigger::Manual];
        if self.on_request {
            triggers.push(Trigger::ReviewRequested);
        }
        json!({
            "name": self.name,
            "description": self.description,
            "prompt": self.prompt,
            "engine": self.engine,
            "skills": self.skills,
            "scope": self.scope(),
            "triggers": triggers,
            "spawn": self.spawn,
            "runs_on": self.runs_on(),
        })
    }

    /// Why it cannot be saved yet, in words.
    pub(crate) fn problem(&self) -> Option<&'static str> {
        if self.name.trim().is_empty() {
            Some("Give the agent a name.")
        } else if self.prompt.trim().is_empty() {
            Some("Write a prompt.")
        } else {
            None
        }
    }
}

// ---- repositories ----

/// Every repository the user has an open change in, or an agent or a read rule names: unique, ignoring case.
pub(crate) fn known_repos(changes: &[Change], agents: &[Agent]) -> Vec<RepoRef> {
    let from_agents = agents.iter().flat_map(|a| match &a.runs_on {
        RunsOn::All => Vec::new(),
        RunsOn::Repos { repos } => repos.clone(),
    });
    let from_changes = changes.iter().map(|c| RepoRef { forge: c.forge, repo: c.repo.clone() });
    let mut found: Vec<RepoRef> = Vec::new();
    for repo in from_changes.chain(from_agents) {
        if !found.iter().any(|r| r.forge == repo.forge && r.repo.eq_ignore_ascii_case(&repo.repo)) {
            found.push(repo);
        }
    }
    found.sort_by(|a, b| (a.repo.to_lowercase(), a.forge.cli()).cmp(&(b.repo.to_lowercase(), b.forge.cli())));
    found
}

/// The place after switching an agent on or off for one repository. Off from "all repos" lists every known repository
/// but this one, so a repository never seen by the app is left out too.
pub(crate) fn with_repo(runs_on: &RunsOn, known: &[RepoRef], target: &RepoRef, on: bool) -> RunsOn {
    let same = |r: &RepoRef| r.forge == target.forge && r.repo.eq_ignore_ascii_case(&target.repo);
    match (runs_on, on) {
        (RunsOn::All, true) => RunsOn::All,
        (RunsOn::All, false) => RunsOn::Repos { repos: known.iter().filter(|r| !same(r)).cloned().collect() },
        (RunsOn::Repos { repos }, true) => {
            let mut repos = repos.clone();
            if !repos.iter().any(same) {
                repos.push(target.clone());
            }
            RunsOn::Repos { repos }
        }
        (RunsOn::Repos { repos }, false) => RunsOn::Repos { repos: repos.iter().filter(|r| !same(r)).cloned().collect() },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use pr_craft_core::agent::Coverage;
    use pr_craft_core::review::LineRef;
    use pr_craft_core::run::{Node, NodeStatus, RunAgent};
    use pr_craft_engine::ai::PromptPreview;

    use super::*;

    /// Draws headless and returns every text the last frame painted, so a test can say what the screen holds. A window
    /// measures itself on its first frame and shows from the second, so it draws three.
    pub(crate) fn drawn(ctx: &Context, mut draw: impl FnMut(&mut craft::egui::Ui)) -> String {
        let mut text = String::new();
        for _ in 0..3 {
            let mut output = ctx.run_ui(craft::egui::RawInput::default(), &mut draw);
            text.clear();
            for clipped in std::mem::take(&mut output.shapes) {
                if let craft::egui::epaint::Shape::Text(shape) = &clipped.shape {
                    text.push_str(shape.galley.text());
                    text.push('\n');
                }
            }
            output.drop_without_applying_deltas();
        }
        text
    }

    pub(crate) fn change(number: u64) -> ChangeRef {
        ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number, host: None }
    }

    pub(crate) fn anchor(sha: &str) -> Anchor {
        Anchor { head_sha: sha.into(), base_sha: None, start_sha: None }
    }

    pub(crate) fn run(id: &str, change_number: u64, state: RunState, started: DateTime<Utc>) -> Run {
        Run {
            id: id.into(),
            change: change(change_number),
            change_title: "Retry webhooks".into(),
            agent: RunAgent { id: Some("a".into()), name: "Concurrency reviewer".into() },
            engine: Engine::ClaudeCode,
            state,
            started,
            ended: (!state.is_active()).then_some(started + chrono::Duration::seconds(30)),
            last_output: None,
            pid: None,
            pid_start: None,
            owner_pid: 1,
            owner_start: None,
            anchor: anchor("abc"),
            files: Vec::new(),
            skipped: Vec::new(),
            redactions: 0,
            limits: SpawnLimits::default(),
            tree: vec![Node {
                id: 0,
                parent: None,
                depth: 0,
                planned: true,
                spawned_by: None,
                reason: None,
                task: "review".into(),
                status: NodeStatus::Running,
                started: Some(started),
                ended: None,
                files_read: Vec::new(),
                findings: 0,
                refusal: None,
            }],
            drafts: Vec::new(),
            dropped: 0,
            long_step: false,
            stopping: false,
            retries: 0,
            error: None,
            tail: Vec::new(),
            extra: Default::default(),
        }
    }

    pub(crate) fn draft(id: &str, path: &str, new: Option<u32>, old: Option<u32>, state: DraftState) -> AiDraft {
        AiDraft {
            id: id.into(),
            finding: Finding { path: path.into(), old_path: None, line: LineRef { old, new }, body: "b".into(), agent: "a".into() },
            state,
        }
    }

    #[test]
    fn a_slash_opens_the_menu_only_where_it_starts_a_word() {
        assert_eq!(slash_query("/", 1), Some((0, String::new())));
        assert_eq!(slash_query("Follow /mi", 10), Some((7, "mi".into())));
        assert_eq!(slash_query("line one\n/fi", 12), Some((9, "fi".into())));
        for (text, cursor) in [("a/b", 3), ("http://x", 8), ("see path/", 9), ("Follow ", 7), ("", 0), ("/x y", 4)] {
            assert_eq!(slash_query(text, cursor), None, "{text:?} at {cursor}");
        }
        // The cursor in the middle of a word reads only up to the cursor; a cursor past the end is clamped.
        assert_eq!(slash_query("/files", 3), Some((0, "fi".into())));
        assert_eq!(slash_query("/é", 99), Some((0, "é".into())));
    }

    #[test]
    fn typing_after_the_slash_filters_the_items_and_inserting_replaces_what_was_typed() {
        assert_eq!(slash_matches("").len(), SLASH_ITEMS.len());
        assert_eq!(slash_matches("fi")[0].insert, "{files}");
        assert_eq!(slash_matches("DIFF")[0].insert, "{diff}");
        assert!(slash_matches("zzz").is_empty());
        // "ase" is inside "base" but starts nothing: it still matches, after prefix matches.
        assert_eq!(slash_matches("ase")[0].insert, "{base_branch}");
        let mut text = String::from("Review /fi now");
        let cursor = apply_slash(&mut text, 7, 10, &SLASH_ITEMS[1]);
        assert_eq!(text, "Review {files} now");
        assert_eq!(cursor, 14);
    }

    #[test]
    fn the_filters_split_runs_by_state_and_today() {
        let now = Utc::now();
        let mut done_before = run("old", 1, RunState::Done, now - chrono::Duration::days(3));
        done_before.ended = Some(now - chrono::Duration::days(3));
        let cases = [
            (run("q", 1, RunState::Queued, now), [true, false, false, false, true]),
            (run("r", 1, RunState::Running, now), [true, false, false, false, true]),
            (run("s", 1, RunState::LooksStuck, now), [false, true, false, false, true]),
            (run("d", 1, RunState::Done, now), [false, false, true, false, true]),
            (run("f", 1, RunState::Failed, now), [false, false, false, true, true]),
            (run("k", 1, RunState::Killed, now), [false, false, false, true, true]),
            (done_before, [false, false, false, false, true]),
        ];
        for (run, want) in cases {
            let got = RunFilter::ALL.map(|f| f.matches(&run, now));
            assert_eq!(got, want, "{} is {:?}", run.id, run.state);
        }
    }

    #[test]
    fn the_pill_counts_one_review_and_flags_a_stuck_run() {
        let now = Utc::now();
        let runs = [
            run("new", 482, RunState::Running, now),
            run("stuck", 482, RunState::LooksStuck, now - chrono::Duration::seconds(40)),
            run("done", 482, RunState::Done, now - chrono::Duration::seconds(60)),
            run("earlier review", 482, RunState::Done, now - chrono::Duration::days(1)),
            run("other change", 7, RunState::Running, now - chrono::Duration::seconds(5)),
        ];
        let pill = pill(&runs).expect("active runs make a pill");
        assert_eq!(pill.change, change(482));
        assert!(pill.text.contains(&change(482).reference()), "{}", pill.text);
        assert!(pill.stuck && pill.detail.contains("1 look stuck"), "{}", pill.detail);
        assert!(super::pill(&[run("d", 1, RunState::Done, now)]).is_none(), "nothing active, no pill");
    }

    #[test]
    fn only_a_finding_on_the_same_line_matches_it_and_pending_drafts_are_ordered() {
        let line = |old, new| DiffLine { kind: pr_craft_core::diff::LineKind::Context, old, new, text: String::new() };
        let on_new = draft("r/0", "a.go", Some(42), None, DraftState::Pending).finding;
        let on_old = draft("r/1", "a.go", None, Some(9), DraftState::Pending).finding;
        assert!(finding_at(&on_new, &line(Some(40), Some(42))) && !finding_at(&on_new, &line(Some(42), None)));
        assert!(finding_at(&on_old, &line(Some(9), None)) && !finding_at(&on_old, &line(Some(9), Some(11))));
        let mut r = run("r", 1, RunState::Done, Utc::now());
        r.drafts = vec![
            draft("r/0", "b.go", Some(3), None, DraftState::Pending),
            draft("r/1", "a.go", Some(50), None, DraftState::Pending),
            draft("r/2", "a.go", Some(5), None, DraftState::Kept),
            draft("r/3", "a.go", Some(7), None, DraftState::Pending),
        ];
        let list = pending(std::iter::once(&r), &anchor("abc"));
        assert_eq!(list.iter().map(|p| p.draft.id.as_str()).collect::<Vec<_>>(), ["r/3", "r/1", "r/0"]);
        assert!(list.iter().all(|p| p.placed));
        assert!(pending(std::iter::once(&r), &anchor("newer")).iter().all(|p| !p.placed), "another commit is not placed on this diff");
        assert_eq!(step_pending(&list, None, true), Some(0));
        assert_eq!(step_pending(&list, Some("r/0"), true), Some(0), "wraps");
        assert_eq!(step_pending(&list, Some("r/3"), false), Some(2), "wraps back");
        assert_eq!(step_pending(&[], None, true), None);
    }

    pub(crate) fn plan(agents: &[(&str, &[&str])], files: &[&str], general: &[&str]) -> Plan {
        let planned: Vec<_> = agents
            .iter()
            .map(|(id, f)| pr_craft_engine::ai::PlannedAgent {
                id: (*id).into(),
                name: id.to_uppercase(),
                engine: Engine::ClaudeCode,
                skills: 0,
                files: f.iter().map(|s| (*s).to_string()).collect(),
            })
            .collect();
        let cells = files
            .iter()
            .map(|p| ((*p).to_string(), agents.iter().filter(|(_, f)| f.contains(p)).map(|(id, _)| (*id).to_string()).collect()))
            .collect();
        Plan {
            change: change(1),
            title: "t".into(),
            anchor: anchor("abc"),
            read: pr_craft_core::redact::ReadRules::default(),
            agents: planned,
            general: general.iter().map(|s| (*s).to_string()).collect(),
            general_engine: Engine::ClaudeCode,
            coverage: Coverage {
                files: files.iter().map(|s| (*s).to_string()).collect(),
                skipped_viewed: Vec::new(),
                agents: agents.iter().map(|(id, _)| (*id).to_string()).collect(),
                cells,
                general: general.iter().map(|s| (*s).to_string()).collect(),
            },
            skipped: Vec::new(),
            blocked: None,
            notes: Vec::new(),
            redaction_notice: String::new(),
        }
    }

    #[test]
    fn the_coverage_grid_has_a_column_per_agent_and_one_for_the_general_pass() {
        let p = plan(&[("go", &["a.go", "b.go"]), ("sql", &["m.sql"]), ("idle", &[])], &["a.go", "b.go", "m.sql", "README.md"], &["README.md"]);
        let grid = Grid::of(&p);
        assert_eq!(grid.columns, ["GO", "SQL", "IDLE", "General"]);
        assert_eq!(grid.rows[0], ("a.go".into(), vec![true, false, false, false]));
        assert_eq!(grid.rows[3], ("README.md".into(), vec![false, false, false, true]));
        assert_eq!(grid.uncovered(), 0, "every file is read by some column");
        assert_eq!(run_count(&p), 3, "two agents with files and the general pass; an agent with none starts no run");
        assert_eq!(run_count(&plan(&[], &["a.md"], &["a.md"])), 1, "no agents: the general pass alone");
    }

    pub(crate) fn prompts(hashes: &[&str]) -> ReviewPreview {
        ReviewPreview {
            change: change(1),
            prompts: hashes
                .iter()
                .map(|h| PromptPreview {
                    agent: RunAgent { id: None, name: "General review".into() },
                    engine: Engine::ClaudeCode,
                    files: Vec::new(),
                    redactions: 0,
                    prompt: "p".into(),
                    prompt_hash: (*h).into(),
                    sent: true,
                    note: None,
                    estimated_tokens: 1,
                })
                .collect(),
            withheld: Vec::new(),
            read: pr_craft_core::redact::ReadRules::default(),
            notice: String::new(),
            estimated_tokens: hashes.len() as u64,
            used_today: 0,
            daily_token_ceiling: None,
        }
    }

    #[test]
    fn start_sends_the_hashes_of_every_previewed_prompt_and_only_for_the_options_previewed() {
        let options = ReviewOptions { skip_viewed: true, agents: Some(vec!["go".into()]), no_general: false };
        let gate = StartGate::of(&prompts(&["aaaa", "bbbb"]), &options);
        assert_eq!(gate.hashes(), ["aaaa", "bbbb"]);
        assert!(gate.matches(&options));
        assert!(!gate.matches(&ReviewOptions::default()), "changing an option makes the preview stale");
        assert!(!gate.matches(&ReviewOptions { skip_viewed: true, agents: None, no_general: false }));
        assert!(!gate.matches(&ReviewOptions { no_general: true, ..options.clone() }));
        let json = options.json(Some(false));
        assert_eq!(json, json!({ "skip_viewed": true, "agents": ["go"], "wait": false }), "the desktop app never waits");
        assert_eq!(ReviewOptions::default().json(None), json!({ "skip_viewed": false }));
    }

    #[test]
    fn the_agent_form_round_trips_its_scope_and_places() {
        let mut form = AgentForm::new(Engine::ClaudeCode, SpawnLimits::default(), None);
        form.name = "Go".into();
        form.prompt = "Review.".into();
        form.include = " **/*.go \n\n".into();
        form.exclude = "*_test.go\n!vendor/**".into();
        assert_eq!(form.scope(), ["**/*.go", "!*_test.go", "!vendor/**"]);
        let agent = Agent {
            id: "go".into(),
            name: form.name.clone(),
            description: String::new(),
            prompt: form.prompt.clone(),
            skills: Vec::new(),
            engine: form.engine,
            scope: form.scope(),
            triggers: vec![Trigger::Manual],
            spawn: SpawnLimits::default(),
            runs_on: RunsOn::All,
            extra: Default::default(),
        };
        let back = AgentForm::from_agent(&agent);
        assert_eq!((back.include.as_str(), back.exclude.as_str()), ("**/*.go", "*_test.go\nvendor/**"));
        assert_eq!(back.scope(), form.scope());
        assert!(back.problem().is_none() && AgentForm { name: " ".into(), ..back }.problem().is_some());
        assert_eq!(form.params()["triggers"], json!(["manual"]));
        form.on_request = true;
        assert_eq!(form.params()["triggers"], json!(["manual", "review_requested"]));
    }

    #[test]
    fn switching_an_agent_off_for_one_repository_keeps_the_others() {
        let repo = |name: &str| RepoRef { forge: Forge::GitHub, repo: name.into() };
        let known = [repo("acme/api"), repo("acme/web")];
        assert_eq!(with_repo(&RunsOn::All, &known, &repo("ACME/web"), false), RunsOn::Repos { repos: vec![repo("acme/api")] });
        assert_eq!(with_repo(&RunsOn::All, &known, &repo("acme/web"), true), RunsOn::All);
        let only_api = RunsOn::Repos { repos: vec![repo("acme/api")] };
        let both = with_repo(&only_api, &known, &repo("acme/web"), true);
        assert_eq!(both, RunsOn::Repos { repos: vec![repo("acme/api"), repo("acme/web")] });
        assert_eq!(with_repo(&both, &known, &repo("acme/web"), true), both, "no duplicate");
        assert_eq!(with_repo(&both, &known, &repo("ACME/API"), false), RunsOn::Repos { repos: vec![repo("acme/web")] });
    }

    #[test]
    fn clock_grows_with_time_never_goes_negative_and_shows_hours_only_when_needed() {
        assert_eq!(clock(-5), clock(0));
        assert_ne!(clock(59), clock(60));
        assert!(clock(3723).len() > clock(460).len(), "an hour adds a field");
        assert_eq!(clock(61).len(), clock(600).len() - 1, "minutes read without padding");
    }

    #[test]
    fn j_and_k_stop_at_the_ends_of_a_list_and_start_at_its_first_row() {
        assert_eq!(nav_step(None, 3, true), Some(0));
        assert_eq!(nav_step(None, 3, false), Some(0));
        assert_eq!(nav_step(Some(0), 3, true), Some(1));
        assert_eq!(nav_step(Some(2), 3, true), Some(2), "no wrap past the last row");
        assert_eq!(nav_step(Some(0), 3, false), Some(0));
        assert_eq!(nav_step(Some(5), 3, false), Some(2), "a selection past a shrunk list lands on its last row");
        assert_eq!(nav_step(Some(0), 0, true), None);
    }

    #[test]
    fn runs_of_a_change_are_the_runs_asked_about_and_a_draft_state_shows_at_once() {
        let now = Utc::now();
        let mut a = run("a", 1, RunState::Done, now);
        a.drafts = vec![draft("a/0", "x.go", Some(1), None, DraftState::Pending)];
        let mut runs = Runs::with(vec![a, run("b", 2, RunState::Running, now)]);
        assert_eq!(runs.of(&change(1)).count(), 1);
        assert_eq!(runs.active().count(), 1);
        runs.set_draft("a/0", DraftState::Kept);
        let kept = runs.find("a").map(|r| r.drafts[0].state);
        assert_eq!(kept, Some(DraftState::Kept));
    }

    #[test]
    fn failed_stopped_and_killed_runs_are_never_counted_as_done() {
        let now = Utc::now();
        let runs = [
            run("a", 1, RunState::Done, now),
            run("b", 1, RunState::Failed, now),
            run("c", 1, RunState::Stopped, now),
            run("d", 1, RunState::Killed, now),
            run("e", 1, RunState::Running, now),
            run("f", 1, RunState::LooksStuck, now),
        ];
        let tally = Tally::of(runs.iter());
        assert_eq!((tally.done, tally.failed, tally.stopped, tally.killed, tally.going, tally.stuck), (1, 1, 1, 1, 2, 1));
        assert_eq!(tally.total(), runs.len());
        let all_good = Tally::of(runs.iter().filter(|r| r.state == RunState::Done));
        assert!(tally.summary().len() > all_good.summary().len(), "the failures show in the words: {}", tally.summary());
        let pilled = pill(&runs).expect("active runs");
        assert_ne!(pilled.text, pill(&[runs[0].clone(), runs[4].clone()]).expect("pill").text, "a failed run changes what the pill says");
    }

    #[test]
    fn keys_act_on_the_stepped_draft_then_the_one_under_the_cursor_then_the_first_current_one() {
        let pending = |id: &str, placed| Pending { draft: draft(id, "a.go", Some(1), None, DraftState::Pending), placed };
        let list = [pending("old", false), pending("cur1", true), pending("cur2", true)];
        assert_eq!(pick_current(&list, None, None), Some(1), "an older-commit draft is not the default");
        assert_eq!(pick_current(&list, None, Some("cur2")), Some(2), "the one under the cursor");
        assert_eq!(pick_current(&list, Some("old"), Some("cur2")), Some(0), "a draft stepped to wins, even an older one");
        assert_eq!(pick_current(&list, Some("gone"), None), Some(1), "a stale focus falls back");
        assert_eq!(pick_current(&[pending("old", false)], None, None), None, "only older drafts: keys do nothing");
    }

    #[test]
    fn only_a_placed_draft_on_a_line_of_the_hunk_is_under_the_cursor() {
        let line = |new| DiffLine { kind: pr_craft_core::diff::LineKind::Added, old: None, new: Some(new), text: String::new() };
        let at = |id: &str, path: &str, n, placed| Pending { draft: draft(id, path, Some(n), None, DraftState::Pending), placed };
        let list = [at("other-file", "b.go", 2, true), at("older", "a.go", 2, false), at("here", "a.go", 2, true), at("below", "a.go", 9, true)];
        let hunk = [line(1), line(2), line(3)];
        assert_eq!(draft_on(&list, "a.go", &hunk).map(|p| p.draft.id.as_str()), Some("here"));
        assert!(draft_on(&list, "a.go", &hunk[..1]).is_none());
    }

    #[test]
    fn a_retry_reads_only_its_own_agent_and_says_what_it_covers_and_costs() {
        let now = Utc::now();
        let mut agent_run = run("r", 482, RunState::LooksStuck, now);
        agent_run.files = vec!["a.go".into(), "b.go".into()];
        let paid = PaidStart::retry(&agent_run, Engine::ClaudeCode);
        assert_eq!(paid.options.agents, Some(vec!["a".to_string()]));
        assert!(paid.options.no_general && !paid.general, "no General review over other agents' files");
        assert_eq!(paid.kill.as_deref(), Some("r"), "Kill and retry ends the stuck run first");
        let said = paid.lines().join("\n");
        for must in [&agent_run.agent.name, "a.go", "usage"] {
            assert!(said.contains(must), "{must} missing in:\n{said}");
        }
        assert_eq!(PaidStart::retry(&run("done", 482, RunState::Failed, now), Engine::ClaudeCode).kill, None, "an ended run has nothing to kill");

        let mut general = run("g", 482, RunState::Failed, now);
        general.agent = RunAgent { id: None, name: "General review".into() };
        let paid = PaidStart::retry(&general, Engine::ClaudeCode);
        assert!(paid.general && !paid.options.no_general);
        assert_ne!(paid.lines(), PaidStart::retry(&agent_run, Engine::ClaudeCode).lines(), "the General review says it covers every file");

        // The old run was on Z.ai, but the agent now uses Claude Code: what the confirm says follows the engine to be used.
        let mut zai = run("z", 482, RunState::Failed, now);
        zai.engine = Engine::ZaiGlm;
        assert!(PaidStart::retry(&zai, Engine::ClaudeCode).lines().join(" ").contains("spends"), "the engine the start will use decides");
        assert!(!PaidStart::retry(&run("c", 482, RunState::Failed, now), Engine::ZaiGlm).lines().join(" ").contains("spends"));
    }

    #[test]
    fn a_start_that_went_only_partway_is_a_warning_and_one_that_started_nothing_is_an_error() {
        let errors = vec!["acme/web #1: nothing to read".to_string()];
        let partial = started_summary("Go", 2, &errors, "⌥⌘J").expect("some started");
        assert!(partial.warn && partial.text.contains("nothing to read"));
        let whole = started_summary("Go", 2, &[], "F2").expect("all started");
        assert!(!whole.warn && whole.text.contains("(F2)"), "the hint shows the key it was given");
        assert!(started_summary("Go", 0, &errors, "⌥⌘J").is_err());
    }
}
