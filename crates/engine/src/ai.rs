//! AI commands: the engine settings, review agents, AI review plans and starts, runs, and AI drafts.
//!
//! An AI run never posts. Its findings are AI drafts: `draft.keep` turns one into a normal draft of the review through
//! the same path `draft.add` takes, and only `review.submit` posts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use pr_craft_core::agent::{
    Agent, Candidate, CoverFile, Coverage, Engine, EngineSettings, GENERAL_PROMPT, PreviewRow, RunsOn, SpawnLimits, StuckRules, Trigger, coverage,
    repo_key, scope_preview,
};
use pr_craft_core::diff::{self, FileDiff};
use pr_craft_core::redact::{REDACTION_NOTICE, ReadRules, ReadScope};
use pr_craft_core::review::{Anchor, LocalReview};
use pr_craft_core::run::{AiDraft, DraftState, Node, NodeStatus, Run, RunAgent, RunState, Skipped};
use pr_craft_core::{ChangeRef, Forge, Utc};
use pr_craft_forge::review as forge;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::review::{self, NewDraft};
use crate::runner::{self, Job, Prompt, PromptInput};
use crate::store::{self, Signal};
use crate::{CommandError, SourceState};

/// The command ids [`command`] takes.
pub const COMMANDS: [&str; 18] = [
    "ai.engine.get",
    "ai.engine.set",
    "ai.read.set",
    "agent.create",
    "agent.update",
    "agent.delete",
    "agent.list",
    "agent.scope_preview",
    "ai.review.plan",
    "ai.review.preview",
    "ai.review.start",
    "run.list",
    "run.get",
    "run.stop",
    "run.kill",
    "run.stop_all",
    "draft.keep",
    "draft.discard",
];

/// Where state lives and which program is the engine.
pub struct Env {
    /// The data directory.
    pub dir: PathBuf,
    /// The Claude Code program: `$PR_CRAFT_CLAUDE`, else `claude` from PATH.
    pub program: PathBuf,
}

impl Env {
    pub fn from_process() -> Result<Env, CommandError> {
        let dir = store::data_dir().map_err(CommandError::Store)?;
        Ok(Env { dir, program: std::env::var_os("PR_CRAFT_CLAUDE").map_or_else(|| PathBuf::from("claude"), PathBuf::from) })
    }
}

/// Serialises every load-change-save of the agents, the settings and the AI drafts of a run in this process.
static AI: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    AI.lock().unwrap_or_else(PoisonError::into_inner)
}

fn store_err(error: String) -> CommandError {
    CommandError::Store(error)
}

fn json<T: Serialize>(value: T) -> Result<Value, CommandError> {
    Ok(serde_json::to_value(value)?)
}

/// Params that may be left out whole (`null`, as the CLI sends without a JSON argument).
fn optional<T: serde::de::DeserializeOwned + Default>(value: &Value) -> Result<T, CommandError> {
    if value.is_null() { Ok(T::default()) } else { crate::params(value) }
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::Invalid(message.into())
}

/// Runs one AI command against the files in `env.dir`.
pub fn command(env: &Env, id: &str, value: &Value) -> Result<Value, CommandError> {
    match id {
        "ai.engine.get" => json(engine_view(env, store::load_ai(&env.dir).map_err(store_err)?)),
        "ai.engine.set" => json(engine_set(env, optional(value)?)?),
        "ai.read.set" => json(read_set(env, crate::params(value)?)?),
        "agent.create" => json(agent_create(env, crate::params(value)?)?),
        "agent.update" => json(agent_update(env, crate::params(value)?)?),
        "agent.delete" => json(agent_delete(env, &crate::params::<AgentId>(value)?.id)?),
        "agent.list" => json(agent_list(env, optional(value)?)?),
        "agent.scope_preview" => json(scope_preview_command(env, crate::params(value)?)?),
        "ai.review.plan" => {
            let p: ReviewParams = crate::params(value)?;
            let source = open_source(&p.change)?;
            json(plan_in(env, &source, &p.options)?)
        }
        "ai.review.preview" => {
            let p: ReviewPreviewParams = crate::params(value)?;
            let source = open_source(&p.change)?;
            json(preview_in(env, &source, p.agent.as_deref(), &p.options)?)
        }
        "ai.review.start" => {
            let p: ReviewParams = crate::params(value)?;
            let source = open_source(&p.change)?;
            let runs = start_checked(env, &source, &p.options, p.expect_hashes.as_deref())?;
            started(env, runs, &p.options)
        }
        "run.list" => json(run_list(env, optional::<RunFilter>(value)?.change.as_ref())),
        "run.get" => json(run_get(env, &crate::params::<RunId>(value)?.run)?),
        "run.stop" => json(run_stop(env, &crate::params::<RunId>(value)?.run)?),
        "run.kill" => json(run_kill(env, &crate::params::<RunId>(value)?.run)?),
        "run.stop_all" => json(run_stop_all(env)?),
        "draft.keep" => {
            let p: DraftOn = crate::params(value)?;
            json(draft_keep(env, &p.change, &p.draft)?)
        }
        "draft.discard" => {
            let p: DraftOn = crate::params(value)?;
            json(draft_discard(env, &p.change, &p.draft)?)
        }
        other => Err(CommandError::Unknown(other.to_string())),
    }
}

// ---- engine settings ----

#[derive(Debug, Serialize, Deserialize)]
pub struct EngineInfo {
    pub engine: Engine,
    pub name: String,
    /// Whether a run on this engine can finish.
    pub wired: bool,
    /// Where the engine program was found.
    pub program: Option<String>,
    /// What the reviewer should know: why it is not usable, or what is missing.
    pub note: Option<String>,
}

/// `ai.engine.get` and `ai.engine.set`: the settings, and what each engine needs.
#[derive(Debug, Serialize, Deserialize)]
pub struct EngineView {
    pub settings: EngineSettings,
    pub engines: Vec<EngineInfo>,
}

fn engine_view(env: &Env, settings: EngineSettings) -> EngineView {
    let claude = find_program(&env.program);
    let engines = Engine::ALL
        .map(|engine| match engine {
            Engine::ClaudeCode => EngineInfo {
                engine,
                name: engine.name().into(),
                wired: true,
                note: claude
                    .is_none()
                    .then(|| format!("`{}` was not found. Install Claude Code, or set PR_CRAFT_CLAUDE to its path.", env.program.display())),
                program: claude.as_ref().map(|path| path.display().to_string()),
            },
            Engine::ZaiGlm => EngineInfo {
                engine,
                name: engine.name().into(),
                wired: false,
                program: None,
                note: Some("Z.ai is not wired yet: its API call and Keychain key are not built, so a run on it fails.".into()),
            },
        })
        .into();
    EngineView { settings, engines }
}

fn find_program(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        return program.is_file().then(|| program.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|folder| folder.join(program)).find(|path| path.is_file())
}

/// `ai.engine.set`: every field is optional; an object field (`stuck`, `spawn`, `read`) replaces that whole group.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnginePatch {
    pub engine: Option<Engine>,
    pub runs_at_once: Option<u32>,
    pub stuck: Option<StuckRules>,
    pub spawn: Option<SpawnLimits>,
    pub read: Option<ReadRules>,
}

fn engine_set(env: &Env, patch: EnginePatch) -> Result<EngineView, CommandError> {
    let _guard = lock();
    let mut settings = store::load_ai(&env.dir).map_err(store_err)?;
    let before = settings.clone();
    settings.engine = patch.engine.unwrap_or(settings.engine);
    settings.runs_at_once = patch.runs_at_once.unwrap_or(settings.runs_at_once);
    settings.stuck = patch.stuck.unwrap_or(settings.stuck);
    settings.spawn = patch.spawn.unwrap_or(settings.spawn);
    settings.read = patch.read.unwrap_or(settings.read);
    if settings.runs_at_once == 0 {
        return Err(invalid("runs at once must be at least 1"));
    }
    if settings.stuck.quiet_secs == 0 || settings.stuck.long_step_secs < settings.stuck.quiet_secs {
        return Err(invalid("the stuck threshold must be above 0, and long steps get at least as long"));
    }
    if settings != before {
        store::save_ai(&env.dir, &settings).map_err(store_err)?;
    }
    Ok(engine_view(env, settings))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadSet {
    forge: Forge,
    repo: String,
    /// The rules for this repository; `null` removes the override.
    rules: Option<ReadRules>,
}

/// `ai.read.set`: replaces the read rules of one repository whole, or removes its override.
fn read_set(env: &Env, p: ReadSet) -> Result<EngineView, CommandError> {
    let _guard = lock();
    let mut settings = store::load_ai(&env.dir).map_err(store_err)?;
    let key = repo_key(p.forge, &p.repo);
    match p.rules {
        Some(rules) => settings.repo_read.insert(key, rules),
        None => settings.repo_read.remove(&key),
    };
    store::save_ai(&env.dir, &settings).map_err(store_err)?;
    Ok(engine_view(env, settings))
}

// ---- agents ----

#[derive(Deserialize)]
struct AgentId {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAgent {
    name: String,
    #[serde(default)]
    description: String,
    prompt: String,
    #[serde(default)]
    skills: Vec<String>,
    /// Defaults to the engine of the settings.
    engine: Option<Engine>,
    #[serde(default)]
    scope: Vec<String>,
    /// Defaults to `["manual"]`.
    triggers: Option<Vec<Trigger>>,
    /// Defaults to the settings' default spawn limits.
    spawn: Option<SpawnLimits>,
    #[serde(default)]
    runs_on: RunsOn,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentPatch {
    id: String,
    name: Option<String>,
    description: Option<String>,
    prompt: Option<String>,
    skills: Option<Vec<String>>,
    engine: Option<Engine>,
    scope: Option<Vec<String>>,
    triggers: Option<Vec<Trigger>>,
    spawn: Option<SpawnLimits>,
    runs_on: Option<RunsOn>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentFilter {
    forge: Option<Forge>,
    repo: Option<String>,
}

fn next_id(prefix: &str) -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    format!("{prefix}-{}-{}-{}", Utc::now().timestamp_millis(), std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed))
}

fn patterns(list: Vec<String>) -> Vec<String> {
    list.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

fn named(text: String, what: &str) -> Result<String, CommandError> {
    let text = text.trim().to_string();
    if text.is_empty() { Err(invalid(format!("an agent needs a {what}"))) } else { Ok(text) }
}

fn agent_create(env: &Env, new: NewAgent) -> Result<Agent, CommandError> {
    let _guard = lock();
    let settings = store::load_ai(&env.dir).map_err(store_err)?;
    let mut agents = store::load_agents(&env.dir).map_err(store_err)?;
    let agent = Agent {
        id: next_id("agent"),
        name: named(new.name, "name")?,
        description: new.description.trim().to_string(),
        prompt: named(new.prompt, "prompt")?,
        skills: patterns(new.skills),
        engine: new.engine.unwrap_or(settings.engine),
        scope: patterns(new.scope),
        triggers: new.triggers.unwrap_or_else(|| vec![Trigger::Manual]),
        spawn: new.spawn.unwrap_or(settings.spawn),
        runs_on: new.runs_on,
    };
    agents.push(agent.clone());
    store::save_agents(&env.dir, &agents).map_err(store_err)?;
    Ok(agent)
}

fn agent_update(env: &Env, patch: AgentPatch) -> Result<Agent, CommandError> {
    let _guard = lock();
    let mut agents = store::load_agents(&env.dir).map_err(store_err)?;
    let agent = agents.iter_mut().find(|agent| agent.id == patch.id).ok_or_else(|| invalid(format!("no agent {}", patch.id)))?;
    if let Some(name) = patch.name {
        agent.name = named(name, "name")?;
    }
    if let Some(prompt) = patch.prompt {
        agent.prompt = named(prompt, "prompt")?;
    }
    agent.description = patch.description.map_or_else(|| agent.description.clone(), |text| text.trim().to_string());
    agent.skills = patch.skills.map_or_else(|| agent.skills.clone(), patterns);
    agent.engine = patch.engine.unwrap_or(agent.engine);
    agent.scope = patch.scope.map_or_else(|| agent.scope.clone(), patterns);
    agent.triggers = patch.triggers.unwrap_or_else(|| agent.triggers.clone());
    agent.spawn = patch.spawn.unwrap_or(agent.spawn);
    agent.runs_on = patch.runs_on.unwrap_or_else(|| agent.runs_on.clone());
    let updated = agent.clone();
    store::save_agents(&env.dir, &agents).map_err(store_err)?;
    Ok(updated)
}

/// Deleting an agent that is already gone is not an error. Runs it made keep their drafts.
fn agent_delete(env: &Env, id: &str) -> Result<Value, CommandError> {
    let _guard = lock();
    let mut agents = store::load_agents(&env.dir).map_err(store_err)?;
    let before = agents.len();
    agents.retain(|agent| agent.id != id);
    let deleted = agents.len() != before;
    if deleted {
        store::save_agents(&env.dir, &agents).map_err(store_err)?;
    }
    Ok(serde_json::json!({ "deleted": deleted }))
}

/// `agent.list`: every agent, or with `forge` and `repo` the ones that run on that repository.
fn agent_list(env: &Env, filter: AgentFilter) -> Result<Vec<Agent>, CommandError> {
    let _guard = lock();
    let agents = store::load_agents(&env.dir).map_err(store_err)?;
    match (filter.forge, filter.repo) {
        (None, None) => Ok(agents),
        (Some(forge), Some(repo)) => Ok(agents.into_iter().filter(|agent| agent.runs_on_repo(forge, &repo)).collect()),
        _ => Err(invalid("filter by repository with both forge and repo")),
    }
}

// ---- scope preview ----

/// An agent by id, or a scope and place not saved yet (the form being edited).
#[derive(Deserialize)]
#[serde(untagged)]
enum AgentSpec {
    Saved(String),
    Unsaved {
        #[serde(default)]
        scope: Vec<String>,
        #[serde(default)]
        runs_on: RunsOn,
    },
}

#[derive(Deserialize)]
struct PreviewParams {
    agent: AgentSpec,
    /// Defaults to every change in the inbox.
    changes: Option<Vec<ChangeRef>>,
}

/// `agent.scope_preview`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Preview {
    pub rows: Vec<PreviewRow>,
    /// Forges whose inbox could not be read, so their changes are missing from `rows`.
    pub unavailable: Vec<Unavailable>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Unavailable {
    pub forge: Forge,
    pub reason: String,
}

/// A file the AI reads: not generated, not binary.
fn reviewable(file: &FileDiff) -> bool {
    !file.generated && !file.binary
}

fn reviewable_paths(change: &ChangeRef) -> Result<Vec<String>, String> {
    let text = forge::diff(change)?;
    Ok(diff::parse_unified(&text).into_iter().filter(reviewable).map(|file| file.path).collect())
}

/// How many diffs are fetched at once.
const FETCHES: usize = 4;

fn candidates(work: Vec<(ChangeRef, String)>, runs_on: &RunsOn) -> Vec<Candidate> {
    let mut out = Vec::with_capacity(work.len());
    for chunk in work.chunks(FETCHES) {
        thread::scope(|scope| {
            let jobs: Vec<_> = chunk
                .iter()
                .map(|(change, _)| {
                    // A repository the agent does not run on needs no diff to say so.
                    scope.spawn(move || if runs_on.includes(change.forge, &change.repo) { reviewable_paths(change) } else { Ok(Vec::new()) })
                })
                .collect();
            for (job, (change, title)) in jobs.into_iter().zip(chunk) {
                let files = job.join().unwrap_or_else(|_| Err("the diff reader stopped".into()));
                out.push(Candidate { change: change.clone(), title: title.clone(), files });
            }
        });
    }
    out
}

fn scope_preview_command(env: &Env, p: PreviewParams) -> Result<Preview, CommandError> {
    let (scope, runs_on) = match p.agent {
        AgentSpec::Unsaved { scope, runs_on } => (patterns(scope), runs_on),
        AgentSpec::Saved(id) => {
            let agent = store::load_agents(&env.dir)
                .map_err(store_err)?
                .into_iter()
                .find(|agent| agent.id == id)
                .ok_or_else(|| invalid(format!("no agent {id}")))?;
            (agent.scope, agent.runs_on)
        }
    };
    let mut unavailable = Vec::new();
    let work = match p.changes {
        Some(changes) => changes.into_iter().map(|change| (change, String::new())).collect(),
        None => {
            let mut work = Vec::new();
            for source in crate::inbox_list() {
                match source.state {
                    SourceState::Listed { changes, .. } => work.extend(changes.into_iter().map(|change| (change.change_ref(), change.title))),
                    SourceState::Paused { login } => {
                        unavailable.push(Unavailable { forge: source.forge, reason: format!("the {} CLI is not signed in ({login:?})", source.cli) })
                    }
                    SourceState::Failed { error } => unavailable.push(Unavailable { forge: source.forge, reason: error }),
                }
            }
            work
        }
    };
    Ok(Preview { rows: scope_preview(&scope, &runs_on, candidates(work, &runs_on)), unavailable })
}

// ---- plan and start ----

/// A change as the plan and the runs read it.
pub struct Source {
    pub change: ChangeRef,
    pub title: String,
    pub base_branch: String,
    pub anchor: Anchor,
    pub files: Vec<FileDiff>,
    /// Paths the reviewer marked viewed.
    pub viewed: BTreeSet<String>,
}

fn open_source(change: &ChangeRef) -> Result<Source, CommandError> {
    let view = review::open(change)?;
    Ok(Source {
        change: view.change,
        title: view.meta.title,
        base_branch: view.meta.base_branch,
        anchor: view.anchor,
        files: view.files,
        viewed: view.local.viewed.into_keys().collect(),
    })
}

#[derive(Deserialize)]
struct ReviewParams {
    change: ChangeRef,
    #[serde(default)]
    options: Options,
    /// The `prompt_hash` of every prompt of the preview the reviewer saw; the start is refused if the prompts changed.
    #[serde(default)]
    expect_hashes: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ReviewPreviewParams {
    change: ChangeRef,
    /// An agent id, or `general` for the general pass; without it, every prompt the review would send.
    agent: Option<String>,
    #[serde(default)]
    options: Options,
}

/// The options of the AI review sheet. There is no duplicate-merging option: it is not built, and an unknown option is refused.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    /// Leave out the files marked viewed.
    pub skip_viewed: bool,
    /// Run only these agents; files of the others go to the general pass. Without it, every agent that runs on the repository.
    pub agents: Option<Vec<String>>,
    /// Whether the files no chosen agent covers go to a general-review run. Default `true`. A retry of one agent passes
    /// `false`, so it reads that agent's files and nothing else.
    pub general: Option<bool>,
    /// Block until every run has ended. Without it, the call returns once the runs have started and they live on threads of
    /// this process, so the desktop app passes `false`. The default is `true`, so a short-lived process (the CLI) never
    /// exits with engines still running.
    pub wait: Option<bool>,
}

impl Options {
    pub fn waits(&self) -> bool {
        self.wait.unwrap_or(true)
    }
}

/// How a run ended, for `ai.review.start` with `wait`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub run: String,
    pub state: RunState,
    pub error: Option<String>,
    pub drafts: usize,
    pub agent: String,
}

fn outcomes(env: &Env, ids: &[String]) -> Vec<Outcome> {
    ids.iter()
        .map(|id| match run_get(env, id) {
            Ok(run) => Outcome { run: run.id, state: run.state, error: run.error, drafts: run.drafts.len(), agent: run.agent.name },
            Err(error) => Outcome { run: id.clone(), state: RunState::Failed, error: Some(error.to_string()), drafts: 0, agent: String::new() },
        })
        .collect()
}

/// The result of `ai.review.start`. A call that waited and in which no run finished is an error, so the CLI exits non-zero.
fn started(env: &Env, runs: Vec<String>, options: &Options) -> Result<Value, CommandError> {
    let results = if options.waits() { Some(outcomes(env, &runs)) } else { None };
    if let Some(results) = &results
        && !results.iter().any(|outcome| outcome.state == RunState::Done || (outcome.state == RunState::Stopped && outcome.drafts > 0))
    {
        return Err(no_run_finished(results));
    }
    json(serde_json::json!({ "runs": runs, "results": results }))
}

/// The error of a waited `ai.review.start` in which no run finished: a non-zero exit for the CLI, with every run's end.
fn no_run_finished(results: &[Outcome]) -> CommandError {
    let lines: Vec<String> = results
        .iter()
        .map(|outcome| {
            let why = outcome.error.as_deref().map_or_else(String::new, |error| format!(": {error}"));
            format!("{} ({}) {:?}{why}", outcome.agent, outcome.run, outcome.state)
        })
        .collect();
    CommandError::Invalid(format!("no AI run finished, so nothing was added. {}", lines.join("; ")))
}

/// One agent's part of the plan.
#[derive(Debug, Serialize, Deserialize)]
pub struct PlannedAgent {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    pub skills: usize,
    /// The files it will read; empty when none of the change's files match its scope.
    pub files: Vec<String>,
}

/// `ai.review.plan`: what the AI review sheet shows.
#[derive(Debug, Serialize, Deserialize)]
pub struct Plan {
    pub change: ChangeRef,
    pub title: String,
    pub anchor: Anchor,
    /// The read rules that apply to this repository.
    pub read: ReadRules,
    pub agents: Vec<PlannedAgent>,
    /// The general pass: files no agent covers.
    pub general: Vec<String>,
    pub general_engine: Engine,
    pub coverage: Coverage,
    /// Files the AI will not read: generated, binary, or never-send.
    pub skipped: Vec<Skipped>,
    /// Why a review cannot start, when it cannot.
    pub blocked: Option<String>,
    /// Things the reviewer should know that are not built or not covered.
    pub notes: Vec<String>,
    /// [`REDACTION_NOTICE`]: what to say next to the read rules, so redaction is never presented as a guarantee.
    pub redaction_notice: String,
}

fn plan_in(env: &Env, source: &Source, options: &Options) -> Result<Plan, CommandError> {
    let settings = store::load_ai(&env.dir).map_err(store_err)?;
    let agents = store::load_agents(&env.dir).map_err(store_err)?;
    Ok(make_plan(&settings, &agents, source, options))
}

fn make_plan(settings: &EngineSettings, agents: &[Agent], source: &Source, options: &Options) -> Plan {
    let rules = settings.read_for(source.change.forge, &source.change.repo);
    let here: Vec<Agent> = agents
        .iter()
        .filter(|agent| agent.runs_on_repo(source.change.forge, &source.change.repo))
        .filter(|agent| options.agents.as_ref().is_none_or(|ids| ids.contains(&agent.id)))
        .cloned()
        .collect();
    let (mut skipped, mut readable) = (Vec::new(), Vec::new());
    for file in &source.files {
        let reason = if file.generated {
            Some("generated file")
        } else if file.binary {
            Some("binary file")
        } else if rules.withheld(&file.path) || file.old_path.as_deref().is_some_and(|old| rules.withheld(old)) {
            Some("matches a never-send pattern")
        } else {
            None
        };
        match reason {
            Some(reason) => skipped.push(Skipped { path: file.path.clone(), reason: reason.into() }),
            None => readable.push(CoverFile { path: file.path.clone(), viewed: source.viewed.contains(&file.path) }),
        }
    }
    let coverage = coverage(&readable, &here, options.skip_viewed);
    let planned = here
        .iter()
        .map(|agent| PlannedAgent {
            id: agent.id.clone(),
            name: agent.name.clone(),
            engine: agent.engine,
            skills: agent.skills.len(),
            files: coverage.files_of(&agent.id),
        })
        .collect();
    let blocked = (rules.scope != ReadScope::DiffOnly).then(|| {
        format!("The read scope {:?} needs a local checkout, which PR Craft does not keep yet. Set it to diff only for this repository.", rules.scope)
    });
    let mut notes = Vec::new();
    if here.iter().any(|agent| !agent.skills.is_empty()) {
        notes.push("Attached skills are not loaded yet: an agent's skills are kept but their text is not in its prompt.".into());
    }
    let uses_zai = |engine: Engine| engine == Engine::ZaiGlm;
    if here.iter().any(|agent| uses_zai(agent.engine)) || (!coverage.general.is_empty() && uses_zai(settings.engine)) {
        notes.push("Z.ai is not wired yet: runs on it fail.".into());
    }
    Plan {
        change: source.change.clone(),
        title: source.title.clone(),
        anchor: source.anchor.clone(),
        read: rules.clone(),
        agents: planned,
        general: coverage.general.clone(),
        general_engine: settings.engine,
        coverage,
        skipped,
        blocked,
        notes,
        redaction_notice: REDACTION_NOTICE.to_string(),
    }
}

/// One run to create: who, on which files, with which prompt.
struct Part {
    agent: RunAgent,
    engine: Engine,
    template: String,
    limits: SpawnLimits,
    files: Vec<String>,
}

/// A run ready to start: its part, the diff it reads and the prompt built from it.
struct Prepared {
    part: Part,
    files: Vec<FileDiff>,
    prompt: Prompt,
}

/// The runs a review would create, each with its prompt built under the read rules. `ai.review.start` and
/// `ai.review.preview` both come through here, so the preview is the prompt that is sent, not a copy of the rules.
fn prepare(settings: &EngineSettings, agents: &[Agent], source: &Source, options: &Options) -> Result<(Plan, Vec<Prepared>), CommandError> {
    let plan = make_plan(settings, agents, source, options);
    if let Some(reason) = &plan.blocked {
        return Err(invalid(reason.clone()));
    }
    if plan.coverage.files.is_empty() {
        return Err(invalid(format!(
            "there is nothing for the AI to read: {} files are generated, binary or never-send, and {} are marked viewed",
            plan.skipped.len(),
            plan.coverage.skipped_viewed.len()
        )));
    }
    let mut parts: Vec<Part> = Vec::new();
    for planned in &plan.agents {
        let Some(agent) = agents.iter().find(|agent| agent.id == planned.id) else { continue };
        if !planned.files.is_empty() {
            parts.push(Part {
                agent: RunAgent { id: Some(agent.id.clone()), name: agent.name.clone() },
                engine: agent.engine,
                template: agent.prompt.clone(),
                limits: agent.spawn,
                files: planned.files.clone(),
            });
        }
    }
    if !plan.general.is_empty() && options.general != Some(false) {
        parts.push(Part {
            agent: RunAgent { id: None, name: "General review".into() },
            engine: settings.engine,
            template: GENERAL_PROMPT.to_string(),
            limits: settings.spawn,
            files: plan.general.clone(),
        });
    }
    if parts.is_empty() {
        return Err(invalid("there is nothing for the chosen agents to read in this change"));
    }
    let rules = settings.read_for(source.change.forge, &source.change.repo);
    let reference = source.change.reference();
    let prepared = parts
        .into_iter()
        .map(|part| {
            let files: Vec<FileDiff> = source.files.iter().filter(|file| part.files.contains(&file.path)).cloned().collect();
            let prompt = runner::build_prompt(&PromptInput {
                template: &part.template,
                reference: &reference,
                title: &source.title,
                base_branch: &source.base_branch,
                files: &files,
                rules,
            });
            Prepared { part, files, prompt }
        })
        .collect();
    Ok((plan, prepared))
}

/// A fingerprint of a prompt: FNV-1a over its bytes, 16 hex digits. It detects a change, nothing more.
fn prompt_hash(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{hash:016x}")
}

/// [`start_in`], refusing when the prompts that would be sent now are not the ones previewed (`expect`: the `prompt_hash`
/// of every prompt of an unfiltered `ai.review.preview`).
pub(crate) fn start_checked(env: &Env, source: &Source, options: &Options, expect: Option<&[String]>) -> Result<Vec<String>, CommandError> {
    let settings = store::load_ai(&env.dir).map_err(store_err)?;
    let agents = store::load_agents(&env.dir).map_err(store_err)?;
    let (_, prepared) = prepare(&settings, &agents, source, options)?;
    if let Some(expected) = expect {
        let mut fresh: Vec<String> = prepared.iter().filter(|p| p.part.engine.wired()).map(|p| prompt_hash(&p.prompt.text)).collect();
        let mut wanted: Vec<String> = expected.iter().filter(|hash| !hash.is_empty()).cloned().collect();
        fresh.sort();
        wanted.sort();
        if fresh != wanted {
            return Err(invalid("the change moved since the preview; preview again"));
        }
    }
    let (mut ids, mut handles) = (Vec::new(), Vec::new());
    for Prepared { part, files, prompt } in prepared {
        let run = new_run(source, &part, &prompt);
        ids.push(run.id.clone());
        if let Some(handle) = begin_run(env, run, prompt.text, files, &settings)? {
            handles.push(handle);
        }
    }
    if options.waits() {
        for handle in handles {
            // A panicked supervisor leaves its run unfinished; `run.list` then marks it failed.
            drop(handle.join());
        }
    }
    Ok(ids)
}

/// One prompt of `ai.review.preview`: exactly the text a run would get on its standard input.
#[derive(Debug, Serialize, Deserialize)]
pub struct PromptPreview {
    pub agent: RunAgent,
    pub engine: Engine,
    /// The files whose diff is in the prompt.
    pub files: Vec<String>,
    /// Secrets masked in this prompt.
    pub redactions: usize,
    /// The whole prompt, after never-send and redaction, byte for byte what `claude` reads. Empty when `sent` is false.
    pub prompt: String,
    /// A fingerprint of `prompt` (16 hex digits). `ai.review.start` takes these back as `expect_hashes`, so a review whose
    /// change moved since the preview is refused. Empty when `sent` is false.
    pub prompt_hash: String,
    /// Whether a run would send this prompt: false for an engine that is not wired.
    pub sent: bool,
    /// "not sent: engine not wired" when `sent` is false.
    pub note: Option<String>,
}

/// `ai.review.preview`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReviewPreview {
    pub change: ChangeRef,
    pub prompts: Vec<PromptPreview>,
    /// Files no prompt carries, and why: never-send, generated, binary.
    pub withheld: Vec<Skipped>,
    /// The read rules that applied.
    pub read: ReadRules,
    /// [`REDACTION_NOTICE`].
    pub notice: String,
}

fn preview_in(env: &Env, source: &Source, agent: Option<&str>, options: &Options) -> Result<ReviewPreview, CommandError> {
    let settings = store::load_ai(&env.dir).map_err(store_err)?;
    let agents = store::load_agents(&env.dir).map_err(store_err)?;
    let (plan, prepared) = prepare(&settings, &agents, source, options)?;
    let picks = |part: &Part| match agent {
        None => true,
        Some("general") => part.agent.id.is_none(),
        Some(id) => part.agent.id.as_deref() == Some(id),
    };
    let prompts: Vec<PromptPreview> = prepared
        .into_iter()
        .filter(|prepared| picks(&prepared.part))
        .map(|Prepared { part, prompt, .. }| {
            let sent = part.engine.wired();
            PromptPreview {
                agent: part.agent,
                engine: part.engine,
                files: prompt.sent,
                redactions: prompt.redactions,
                prompt_hash: if sent { prompt_hash(&prompt.text) } else { String::new() },
                prompt: if sent { prompt.text } else { String::new() },
                sent,
                note: (!sent).then(|| "not sent: engine not wired".to_string()),
            }
        })
        .collect();
    if prompts.is_empty()
        && let Some(id) = agent
    {
        return Err(invalid(format!(
            "agent `{id}` has no files in this change, so it has no prompt to preview (use `general` for the general pass)"
        )));
    }
    Ok(ReviewPreview { change: source.change.clone(), prompts, withheld: plan.skipped, read: plan.read, notice: REDACTION_NOTICE.to_string() })
}

fn new_run(source: &Source, part: &Part, prompt: &Prompt) -> Run {
    let now = Utc::now();
    Run {
        id: next_id("run"),
        change: source.change.clone(),
        change_title: source.title.clone(),
        agent: part.agent.clone(),
        engine: part.engine,
        state: RunState::Queued,
        started: now,
        ended: None,
        last_output: None,
        pid: None,
        pid_start: None,
        owner_pid: std::process::id(),
        owner_start: runner::process_started(std::process::id()),
        anchor: source.anchor.clone(),
        files: prompt.sent.clone(),
        skipped: prompt.skipped.clone(),
        redactions: u32::try_from(prompt.redactions).unwrap_or(u32::MAX),
        limits: part.limits,
        tree: vec![Node {
            id: 0,
            parent: None,
            depth: 0,
            planned: true,
            spawned_by: None,
            reason: None,
            task: prompt.sent.join(", "),
            status: NodeStatus::Queued,
            started: None,
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
    }
}

/// Saves the run and starts it, unless its engine cannot run: then it is saved as failed, so the reviewer sees why.
fn begin_run(
    env: &Env,
    mut run: Run,
    prompt: String,
    files: Vec<FileDiff>,
    settings: &EngineSettings,
) -> Result<Option<JoinHandle<()>>, CommandError> {
    if !run.engine.wired() {
        let now = Utc::now();
        run.state = RunState::Failed;
        run.ended = Some(now);
        run.error =
            Some(format!("{} is not wired yet: its API call and Keychain key are not built. Pick Claude Code for this agent.", run.engine.name()));
        if let Some(root) = run.tree.first_mut() {
            root.status = NodeStatus::Failed;
            root.ended = Some(now);
        }
        store::create_run(&env.dir, &run).map_err(store_err)?;
        return Ok(None);
    }
    let job = Job { dir: env.dir.clone(), program: env.program.clone(), run, prompt, files, settings: settings.clone() };
    runner::launch(job).map(Some).map_err(CommandError::Store)
}

// ---- runs ----

#[derive(Deserialize)]
struct RunId {
    run: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunFilter {
    change: Option<ChangeRef>,
}

/// `run.list`: the runs, newest first, with the files that could not be read.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunList {
    pub runs: Vec<Run>,
    pub unreadable: Vec<String>,
}

/// Whether the process that supervises `run` is still going: this process with the run's thread alive, or another PR
/// Craft process that is still the one that started it (a reused pid is not).
fn supervised(run: &Run) -> bool {
    if run.owner_pid == std::process::id() { runner::is_live(&run.id) } else { runner::owner_alive(run.owner_pid, run.owner_start.as_deref()) }
}

/// Ends a run whose supervisor is gone, and the engine it left running if that process is verified to be ours.
fn settle_orphan(run: &mut Run, killed: bool) {
    if let Some(pid) = run.pid {
        if runner::kill_verified(pid, run.pid_start.as_deref()) {
            run.push_tail(format!("Ended the engine process {pid} that was left running."));
        } else if runner::process_started(pid).is_some() {
            run.push_tail(format!("Process {pid} was left alone: it could not be verified as this run's engine."));
        }
    }
    let now = Utc::now();
    run.state = if killed { RunState::Killed } else { RunState::Failed };
    run.error = (!killed).then(|| "PR Craft stopped while this run was going, so its result is lost. Start the review again.".to_string());
    run.ended = Some(now);
    if let Some(root) = run.tree.first_mut() {
        root.status = NodeStatus::Stopped;
        root.ended = Some(now);
    }
}

/// A run that looks active but whose supervisor is gone (PR Craft quit or crashed) can never finish: it ends as failed.
/// The decision is taken again on the file as it is under its lock, so a run that finished meanwhile is never overwritten,
/// and `run` is replaced by that fresh copy.
fn reconcile(env: &Env, run: &mut Run) {
    if !run.state.is_active() || supervised(run) {
        return;
    }
    let settled = store::with_run(&env.dir, &run.id, |disk| {
        if disk.state.is_active() && !supervised(disk) {
            settle_orphan(disk, store::read_signal(&env.dir, &disk.id) == Some(Signal::Kill));
            store::clear_signal(&env.dir, &disk.id);
        }
    });
    match settled {
        Ok(Some((fresh, ()))) => *run = fresh,
        Ok(None) => {}
        Err(error) => eprintln!("pr-craft: could not settle run {}: {error}", run.id),
    }
}

fn run_list(env: &Env, change: Option<&ChangeRef>) -> RunList {
    let (mut runs, unreadable) = store::list_runs(&env.dir);
    runs.iter_mut().for_each(|run| reconcile(env, run));
    runs.retain(|run| change.is_none_or(|change| run.change == *change));
    RunList { runs, unreadable }
}

fn run_get(env: &Env, id: &str) -> Result<Run, CommandError> {
    let mut run = store::load_run(&env.dir, id).map_err(store_err)?.ok_or_else(|| invalid(format!("no run {id}")))?;
    reconcile(env, &mut run);
    Ok(run)
}

fn active_run(env: &Env, id: &str) -> Result<Run, CommandError> {
    let run = run_get(env, id)?;
    if run.state.is_active() { Ok(run) } else { Err(invalid(format!("run {id} has already ended ({:?})", run.state))) }
}

/// `run.stop`: the engine has one step, writing its answer, so Stop ends it now. The run ends `stopped`; findings from an
/// assistant message that had already finished are kept, a message still being written is not. A queued run stops at once.
fn run_stop(env: &Env, id: &str) -> Result<Run, CommandError> {
    let mut run = active_run(env, id)?;
    store::send_signal(&env.dir, id, Signal::Stop).map_err(store_err)?;
    match store::with_run(&env.dir, id, |disk| disk.stopping |= disk.state.is_active()).map_err(store_err)? {
        Some((fresh, ())) => Ok(fresh),
        None => {
            run.stopping = true;
            Ok(run)
        }
    }
}

/// `run.kill`: ends the engine process now. The pid is signalled only while it is verified to be this run's engine.
fn run_kill(env: &Env, id: &str) -> Result<Run, CommandError> {
    let run = active_run(env, id)?;
    store::send_signal(&env.dir, id, Signal::Kill).map_err(store_err)?;
    if let Some(pid) = run.pid {
        // The supervisor sees the marker too; this reaches an engine whose supervisor is slow or in another process.
        runner::kill_verified(pid, run.pid_start.as_deref());
    }
    Ok(run)
}

/// `run.stop_all`: Stop for every run that is queued, running or looks stuck.
fn run_stop_all(env: &Env) -> Result<Value, CommandError> {
    let mut stopped = Vec::new();
    for run in run_list(env, None).runs.into_iter().filter(|run| run.state.is_active()) {
        store::send_signal(&env.dir, &run.id, Signal::Stop).map_err(store_err)?;
        store::with_run(&env.dir, &run.id, |disk| disk.stopping |= disk.state.is_active()).map_err(store_err)?;
        stopped.push(run.id);
    }
    Ok(serde_json::json!({ "stopped": stopped }))
}

// ---- AI drafts ----

#[derive(Deserialize)]
struct DraftOn {
    change: ChangeRef,
    /// An [`AiDraft::id`].
    draft: String,
}

/// `draft.keep`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Kept {
    /// The review as it is now, with the new draft in it.
    pub local: LocalReview,
    /// The id of the new draft among `local.drafts`.
    pub draft: u64,
    pub ai_draft: AiDraft,
}

/// The position of the AI draft `id` in a run of `change`, while it is still pending.
fn pending_draft(run: &Run, change: &ChangeRef, id: &str) -> Result<usize, CommandError> {
    if run.change != *change {
        return Err(invalid(format!("AI draft {id} belongs to {}, not {}", run.change.reference(), change.reference())));
    }
    let at = run.drafts.iter().position(|draft| draft.id == id).ok_or_else(|| invalid(format!("no AI draft {id}")))?;
    match run.drafts.get(at).map(|draft| draft.state) {
        Some(DraftState::Pending) => Ok(at),
        Some(DraftState::Kept) => Err(invalid(format!("AI draft {id} was already kept"))),
        _ => Err(invalid(format!("AI draft {id} was discarded"))),
    }
}

/// Changes the state of one AI draft under the run's lock, on the run as it is now: the supervisor's fields are not touched.
fn settle_draft<T>(
    env: &Env,
    change: &ChangeRef,
    id: &str,
    act: impl FnOnce(&mut AiDraft, &Run) -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    let (run_id, _) = id.rsplit_once('/').ok_or_else(|| invalid(format!("`{id}` is not an AI draft id")))?;
    let changed = store::with_run(&env.dir, run_id, |run| {
        let at = pending_draft(run, change, id)?;
        let snapshot = run.clone();
        let draft = run.drafts.get_mut(at).ok_or_else(|| invalid("no such AI draft"))?;
        act(draft, &snapshot)
    });
    match changed {
        Ok(Some((_, outcome))) => outcome,
        Ok(None) => Err(invalid(format!("no run {run_id}"))),
        Err(error) => Err(CommandError::Store(error)),
    }
}

/// `draft.keep`: the AI draft becomes a draft of the review, by the path `draft.add` takes, so the anchor rules apply.
/// Nothing posts; `review.submit` still does that.
fn draft_keep(env: &Env, change: &ChangeRef, id: &str) -> Result<Kept, CommandError> {
    settle_draft(env, change, id, |ai_draft, run| {
        let finding = ai_draft.finding.clone();
        let new = NewDraft {
            path: finding.path,
            old_path: finding.old_path,
            start: None,
            end: finding.line,
            body: finding.body,
            anchor: run.anchor.clone(),
        };
        // A retry after a failed save of the run finds the draft the first try added, and reuses it instead of adding another.
        let earlier = store::load_review(&env.dir, change).map_err(store_err)?;
        let added = earlier.drafts.iter().find(|draft| draft.path == new.path && draft.end == new.end && draft.body == new.body);
        let (local, draft) = match added {
            Some(draft) if earlier.anchor.as_ref() == Some(&new.anchor) => (earlier.clone(), draft.id),
            _ => {
                let local = review::draft_add_in(&env.dir, change, new)?;
                let draft = local
                    .drafts
                    .last()
                    .map(|draft| draft.id)
                    .ok_or_else(|| CommandError::Store("the draft was added but is not in the review".into()))?;
                (local, draft)
            }
        };
        ai_draft.state = DraftState::Kept;
        Ok(Kept { local, draft, ai_draft: ai_draft.clone() })
    })
}

/// `draft.discard`.
fn draft_discard(env: &Env, change: &ChangeRef, id: &str) -> Result<AiDraft, CommandError> {
    settle_draft(env, change, id, |ai_draft, _| {
        ai_draft.state = DraftState::Discarded;
        Ok(ai_draft.clone())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use pr_craft_core::diff::parse_unified;
    use pr_craft_core::redact::MASK;
    use serde_json::json;

    use super::*;

    /// [`start_checked`] without an expected preview, as most tests start a review.
    fn start_in(env: &Env, source: &Source, options: &Options) -> Result<Vec<String>, CommandError> {
        start_checked(env, source, options, None)
    }

    const DIFF: &str = "diff --git a/a.go b/a.go
index 1111111..2222222 100644
--- a/a.go
+++ b/a.go
@@ -1,3 +1,4 @@
 package a
+var key = \"ghp_0123456789abcdef0123456789abcdef0123\"
+func f() {}
 func g() {}
diff --git a/deploy/.env b/deploy/.env
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/deploy/.env
@@ -0,0 +1 @@
+DB_HOST=internal-db-9
";

    fn change() -> ChangeRef {
        ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 482, host: None }
    }

    fn source(diff: &str) -> Source {
        Source {
            change: change(),
            title: "Retry webhooks".into(),
            base_branch: "main".into(),
            anchor: Anchor { head_sha: "h1".into(), base_sha: None, start_sha: None },
            files: parse_unified(diff),
            viewed: BTreeSet::new(),
        }
    }

    fn root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pr-craft-ai-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// A stand-in for `claude`: a shell script that records what it was given and prints canned output.
    fn fake_engine(root: &Path, body: &str) -> PathBuf {
        let path = root.join("claude");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the fake engine");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make it executable");
        path
    }

    /// Records the prompt and the arguments, then streams a read step and the given answer.
    fn answering(root: &Path, answer: &str) -> String {
        let read = r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#;
        let result = json!({ "type": "result", "subtype": "success", "is_error": false, "result": answer }).to_string();
        let dir = root.display();
        format!(
            "cat > '{dir}/prompt.txt'\nprintf '%s\\n' \"$@\" > '{dir}/args.txt'\ncat <<'EOF'\n{{\"type\":\"system\",\"subtype\":\"init\"}}\n{read}\n{{\"type\":\"user\",\"message\":{{}}}}\n{result}\nEOF"
        )
    }

    fn env_with(root: &Path, program: PathBuf) -> Env {
        Env { dir: root.join("data"), program }
    }

    fn waiting() -> Options {
        Options { wait: Some(true), ..Options::default() }
    }

    fn detached() -> Options {
        Options { wait: Some(false), ..Options::default() }
    }

    fn runs(env: &Env) -> Vec<Run> {
        run_list(env, None).runs
    }

    fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
        for _ in 0..150 {
            if ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }

    fn new_agent(env: &Env, scope: &[&str]) -> Agent {
        let made = command(env, "agent.create", &json!({ "name": "Go idioms", "prompt": "Review Go changes.", "scope": scope })).expect("create");
        serde_json::from_value(made).expect("an agent")
    }

    #[test]
    fn a_run_ends_done_with_its_valid_findings_as_ai_drafts() {
        let root = root("done");
        let answer = json!([
            { "path": "a.go", "line": 3, "body": "A token must not live in source." },
            { "path": "a.go", "line": 99, "body": "This line is not in the diff." }
        ])
        .to_string();
        let env = env_with(&root, fake_engine(&root, &answering(&root, &answer)));
        new_agent(&env, &["**/*.go"]);

        let ids = start_in(&env, &source(DIFF), &Options::default()).expect("start");
        assert_eq!(ids.len(), 1, "one agent covers a.go; the .env file is never sent");
        let listed = runs(&env);
        let run = listed.iter().find(|run| run.id == ids[0]).expect("the run is listed");
        assert_eq!(run.state, RunState::Done, "{:?}", run.error);
        assert_eq!(run.drafts.len(), 1);
        assert_eq!(run.dropped, 1, "the finding on a line outside the diff is dropped and counted");
        assert_eq!(run.drafts[0].state, DraftState::Pending);
        assert!(run.tree[0].files_read.is_empty(), "the engine has no tools, so it reads no files");
        assert_eq!(run.tree[0].findings, 1);
        assert!(run.pid.is_some() && run.ended.is_some() && run.last_output.is_some());

        let prompt = fs::read_to_string(root.join("prompt.txt")).expect("the engine got the prompt on stdin");
        assert!(prompt.contains("=== a.go") && prompt.contains(MASK));
        assert!(!prompt.contains("ghp_0123") && !prompt.contains("internal-db-9"), "nothing secret and no never-send file reached the engine");
        let args = fs::read_to_string(root.join("args.txt")).expect("args");
        assert_eq!(args.lines().collect::<Vec<_>>(), runner::CLAUDE_ARGS, "the engine runs with no tools, as documented");
        assert!(run.redactions >= 1);
        assert!(!root.join("data/sandbox").join(&run.id).exists(), "the empty working folder is removed");
    }

    #[test]
    fn the_preview_is_byte_for_byte_the_prompt_the_engine_reads() {
        let root = root("preview");
        let env = env_with(&root, fake_engine(&root, &answering(&root, "[]")));
        let go = new_agent(&env, &["**/*.go"]);
        let src = source(DIFF);

        let preview = preview_in(&env, &src, None, &Options::default()).expect("preview");
        assert_eq!(preview.prompts.len(), 1);
        assert!(preview.prompts[0].redactions >= 1, "the masked spans are counted");
        assert!(preview.withheld.iter().any(|file| file.path == "deploy/.env"), "a never-send file is listed as withheld");
        assert!(!preview.prompts[0].prompt.contains("ghp_0123") && !preview.prompts[0].prompt.contains("internal-db-9"));
        assert!(preview.notice.contains("Preview exactly what is sent"));

        start_in(&env, &src, &Options::default()).expect("start");
        let sent = fs::read_to_string(root.join("prompt.txt")).expect("what the engine read on stdin");
        assert_eq!(sent, preview.prompts[0].prompt, "the preview is the prompt that was sent");

        let one = preview_in(&env, &src, Some(&go.id), &Options::default()).expect("one agent");
        assert_eq!(one.prompts.len(), 1);
        assert!(preview_in(&env, &src, Some("general"), &Options::default()).is_err(), "no file goes to the general pass here");
        assert!(preview_in(&env, &src, Some("nope"), &Options::default()).is_err(), "an unknown agent is not an empty preview");
    }

    #[test]
    fn a_start_without_the_general_pass_reads_only_the_chosen_agents_files() {
        let root = root("no-general");
        let env = env_with(&root, fake_engine(&root, &answering(&root, "[]")));
        let go = new_agent(&env, &["**/*.go"]);
        let diff = format!("{DIFF}diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n # T\n+More\n");
        let src = source(&diff);
        let only = Options { agents: Some(vec![go.id.clone()]), general: Some(false), ..Options::default() };
        let preview = preview_in(&env, &src, None, &only).expect("preview");
        assert_eq!(preview.prompts.len(), 1, "no general run for README.md");
        assert!(!preview.prompts[0].files.iter().any(|f| f == "README.md"));
        let with = preview_in(&env, &src, None, &Options { general: None, ..only }).expect("preview");
        assert!(with.prompts.len() > preview.prompts.len(), "by default the left-over file is reviewed by a general run");
        let nothing = Options { agents: Some(Vec::new()), general: Some(false), ..Options::default() };
        assert!(preview_in(&env, &src, None, &nothing).is_err(), "no agent and no general pass is an error, not an empty start");
    }

    #[test]
    fn a_start_is_refused_when_the_change_moved_since_the_preview() {
        let root = root("hashes");
        let env = env_with(&root, fake_engine(&root, &answering(&root, "[]")));
        new_agent(&env, &["**/*.go"]);
        let src = source(DIFF);
        let preview = preview_in(&env, &src, None, &Options::default()).expect("preview");
        let hashes: Vec<String> = preview.prompts.iter().map(|prompt| prompt.prompt_hash.clone()).collect();
        assert!(hashes.iter().all(|hash| hash.len() == 16));

        // A push changed the diff after the reviewer looked.
        let moved = source(&DIFF.replace("func f() {}", "func f() { go work() }"));
        let refused = start_checked(&env, &moved, &Options::default(), Some(&hashes)).expect_err("the prompt differs");
        assert!(refused.to_string().contains("the change moved since the preview; preview again"), "{refused}");
        assert!(runs(&env).is_empty(), "nothing was started");

        assert!(start_checked(&env, &src, &Options::default(), Some(&hashes)).is_ok(), "the same change starts");
        assert_eq!(runs(&env).len(), 1);
    }

    #[test]
    fn the_preview_of_an_engine_that_is_not_wired_says_it_is_not_sent() {
        let root = root("preview-zai");
        let env = env_with(&root, root.join("never-run"));
        command(&env, "agent.create", &json!({ "name": "Z", "prompt": "Review.", "engine": "zai_glm" })).expect("create");
        let preview = preview_in(&env, &source(DIFF), None, &Options::default()).expect("preview");
        let entry = &preview.prompts[0];
        assert!(!entry.sent && entry.prompt.is_empty() && entry.prompt_hash.is_empty());
        assert_eq!(entry.note.as_deref(), Some("not sent: engine not wired"));
        assert!(!entry.files.is_empty(), "the files it would have read are still listed");
    }

    #[test]
    fn a_waited_start_counts_a_stopped_run_that_kept_drafts_as_finished() {
        let root = root("stopped-drafts");
        let env = env_with(&root, PathBuf::from("unused"));
        let outcome = |state, drafts| Outcome { run: "r".into(), state, error: None, drafts, agent: "a".into() };
        assert!(no_run_finished(&[outcome(RunState::Failed, 0)]).to_string().contains("no AI run finished"));
        let run = orphan_run(&env, None);
        store::with_run(&env.dir, &run.id, |disk| {
            disk.state = RunState::Stopped;
            disk.drafts = vec![AiDraft { id: format!("{}/0", disk.id), finding: dummy_finding(), state: DraftState::Pending }];
        })
        .expect("save");
        assert!(started(&env, vec![run.id], &waiting()).is_ok());
    }

    #[test]
    fn keeping_an_ai_draft_adds_a_normal_draft_and_posts_nothing() {
        let root = root("keep");
        let answer = json!([{ "path": "a.go", "line": 3, "body": "A token must not live in source." }]).to_string();
        let env = env_with(&root, fake_engine(&root, &answering(&root, &answer)));
        new_agent(&env, &[]);
        let ids = start_in(&env, &source(DIFF), &waiting()).expect("start");
        let run = run_get(&env, &ids[0]).expect("run");
        let draft_id = run.drafts[0].id.clone();

        let kept = draft_keep(&env, &change(), &draft_id).expect("keep");
        assert_eq!(kept.local.drafts.len(), 1);
        let draft = &kept.local.drafts[0];
        assert_eq!((draft.path.as_str(), draft.end.new), ("a.go", Some(3)));
        assert_eq!(draft.body, "A token must not live in source.");
        assert_eq!(kept.local.anchor, Some(run.anchor.clone()), "it anchors to the commit the AI read");
        assert_eq!(store::load_review(&env.dir, &change()).expect("saved").drafts.len(), 1, "it is in the review on this Mac");
        assert_eq!(run_get(&env, &run.id).expect("run").drafts[0].state, DraftState::Kept);
        assert!(draft_keep(&env, &change(), &draft_id).is_err(), "a draft is kept once");
        assert!(draft_discard(&env, &change(), &draft_id).is_err(), "and a kept one cannot be discarded");
        assert_eq!(store::load_review(&env.dir, &change()).expect("saved").drafts.len(), 1);

        // The run could not be saved after the draft was added: the AI draft is still pending, and keeping it again adds nothing.
        store::with_run(&env.dir, &run.id, |disk| disk.drafts[0].state = DraftState::Pending).expect("reset");
        let again = draft_keep(&env, &change(), &draft_id).expect("keep again");
        assert_eq!((again.draft, again.local.drafts.len()), (kept.draft, 1), "a retry reuses the draft the first try added");
        assert_eq!(run_get(&env, &run.id).expect("run").drafts[0].state, DraftState::Kept);
    }

    #[test]
    fn a_kept_draft_obeys_the_anchor_rules_and_a_discarded_one_stays_out() {
        let root = root("anchor");
        let env = env_with(&root, PathBuf::from("unused"));
        let src = source(DIFF);
        let part = Part {
            agent: RunAgent { id: None, name: "General review".into() },
            engine: Engine::ClaudeCode,
            template: String::new(),
            limits: SpawnLimits::default(),
            files: Vec::new(),
        };
        let prompt = Prompt { text: String::new(), sent: vec!["a.go".into()], skipped: Vec::new(), redactions: 0 };
        let mut run = new_run(&src, &part, &prompt);
        run.state = RunState::Done;
        let finding = |body: &str| pr_craft_core::run::Finding {
            path: "a.go".into(),
            old_path: None,
            line: pr_craft_core::review::LineRef { old: None, new: Some(3) },
            body: body.into(),
            agent: "General review".into(),
        };
        run.drafts = vec![
            AiDraft { id: format!("{}/0", run.id), finding: finding("keep me"), state: DraftState::Pending },
            AiDraft { id: format!("{}/1", run.id), finding: finding("drop me"), state: DraftState::Pending },
        ];
        store::create_run(&env.dir, &run).expect("save");
        review::draft_add_in(
            &env.dir,
            &change(),
            NewDraft {
                path: "b.go".into(),
                old_path: None,
                start: None,
                end: pr_craft_core::review::LineRef { old: None, new: Some(1) },
                body: "mine".into(),
                anchor: Anchor { head_sha: "older".into(), base_sha: None, start_sha: None },
            },
        )
        .expect("a draft on an older commit");

        let refused = draft_keep(&env, &change(), &run.drafts[0].id).expect_err("another commit");
        assert!(refused.to_string().contains("new commits"), "{refused}");
        assert_eq!(run_get(&env, &run.id).expect("run").drafts[0].state, DraftState::Pending, "a refused keep changes nothing");

        let discarded = draft_discard(&env, &change(), &run.drafts[1].id).expect("discard");
        assert_eq!(discarded.state, DraftState::Discarded);
        assert_eq!(store::load_review(&env.dir, &change()).expect("saved").drafts.len(), 1, "a discarded AI draft never reaches the review");
        let other = ChangeRef { number: 1, ..change() };
        assert!(draft_keep(&env, &other, &run.drafts[0].id).is_err(), "an AI draft is kept on its own change only");
    }

    #[test]
    fn an_engine_that_fails_leaves_a_failed_run_that_says_why() {
        let root = root("fails");
        let env = env_with(&root, fake_engine(&root, "echo 'Claude usage limit reached' >&2\nexit 3"));
        new_agent(&env, &[]);
        let ids = start_in(&env, &source(DIFF), &waiting()).expect("start");
        let run = run_get(&env, &ids[0]).expect("run");
        assert_eq!(run.state, RunState::Failed);
        assert!(run.error.as_deref().is_some_and(|e| e.contains("usage limit")), "{:?}", run.error);
        assert!(run.drafts.is_empty());
        let refused = started(&env, ids.clone(), &waiting()).expect_err("no run finished");
        assert!(refused.to_string().contains("usage limit") && refused.to_string().contains(&ids[0]), "{refused}");
        assert!(started(&env, ids, &detached()).is_ok(), "a call that did not wait has nothing to judge yet");

        let said = json!({ "type": "result", "subtype": "success", "is_error": true, "result": "Credit balance is too low" }).to_string();
        let env = env_with(&root, fake_engine(&root, &format!("cat >/dev/null\necho '{said}'")));
        let ids = start_in(&env, &source(DIFF), &waiting()).expect("start");
        let run = run_get(&env, &ids[0]).expect("run");
        assert_eq!((run.state, run.error.as_deref()), (RunState::Failed, Some("Credit balance is too low")));

        let missing = env_with(&root, root.join("no-such-claude"));
        let ids = start_in(&missing, &source(DIFF), &waiting()).expect("start");
        assert!(run_get(&missing, &ids[0]).expect("run").error.is_some_and(|e| e.contains("could not start")), "a missing CLI is said, not hidden");

        let say = json!({ "type": "result", "subtype": "success", "is_error": false, "result": "All fine, nothing to report." }).to_string();
        let env = env_with(&root, fake_engine(&root, &format!("cat >/dev/null\necho '{say}'")));
        let ids = start_in(&env, &source(DIFF), &waiting()).expect("start");
        let run = run_get(&env, &ids[0]).expect("run");
        assert_eq!(run.state, RunState::Failed, "an answer with no findings list is not an empty review");
    }

    #[test]
    fn a_run_on_the_engine_that_is_not_wired_fails_at_once_and_says_so() {
        let root = root("zai");
        let env = env_with(&root, root.join("never-run"));
        command(&env, "agent.create", &json!({ "name": "Z", "prompt": "Review.", "engine": "zai_glm" })).expect("create");
        let ids = start_in(&env, &source(DIFF), &waiting()).expect("start");
        let run = run_get(&env, &ids[0]).expect("run");
        assert_eq!(run.state, RunState::Failed);
        assert!(run.error.is_some_and(|e| e.contains("not wired")));
        let plan = plan_in(&env, &source(DIFF), &Options::default()).expect("plan");
        assert!(plan.notes.iter().any(|note| note.contains("Z.ai")), "the sheet warns before the run");
        let view = command(&env, "ai.engine.get", &Value::Null).expect("get");
        assert_eq!(view["engines"][1]["wired"], json!(false));
    }

    fn message(text: &str) -> String {
        json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": text }] } }).to_string()
    }

    /// The engine, started in the background and running, as the reviewer sees a review that has not finished.
    fn running_engine(name: &str, script: &str) -> (Env, String) {
        let root = root(name);
        let env = env_with(&root, fake_engine(&root, script));
        new_agent(&env, &[]);
        let ids = start_in(&env, &source(DIFF), &detached()).expect("start");
        let id = ids[0].clone();
        wait_until("the engine to start", || run_get(&env, &id).is_ok_and(|run| run.pid.is_some() && run.pid_start.is_some()));
        (env, id)
    }

    #[test]
    fn kill_ends_the_engine_process_now() {
        let (env, id) = running_engine("kill", "exec sleep 30");
        let pid = run_get(&env, &id).expect("run").pid.expect("pid");
        assert!(runner::process_started(pid).is_some());

        command(&env, "run.kill", &json!({ "run": id })).expect("kill");
        wait_until("the run to be killed", || run_get(&env, &id).is_ok_and(|run| run.state == RunState::Killed));
        assert!(runner::process_started(pid).is_none(), "the process is gone");
        assert!(command(&env, "run.kill", &json!({ "run": id })).is_err(), "an ended run cannot be killed again");
    }

    #[test]
    fn stop_works_before_the_engine_has_said_anything() {
        let (env, id) = running_engine("stop", "cat >/dev/null\necho '{\"type\":\"system\",\"subtype\":\"init\"}'\nexec sleep 30");
        let stopping = command(&env, "run.stop", &json!({ "run": id })).expect("stop");
        assert_eq!(stopping["stopping"], json!(true));
        wait_until("the run to stop", || run_get(&env, &id).is_ok_and(|run| run.state == RunState::Stopped));
        let run = run_get(&env, &id).expect("run");
        assert!(run.drafts.is_empty() && run.error.is_none(), "a message not yet written adds nothing and is not an error");
        let pid = run.pid.expect("pid");
        assert!(runner::process_started(pid).is_none(), "the engine process was ended");
    }

    #[test]
    fn stop_keeps_what_the_engine_had_finished() {
        let found = message(r#"[{"path":"a.go","line":"L2","body":"Token in source."}]"#);
        let (env, id) = running_engine("stop-keeps", &format!("cat >/dev/null\necho '{found}'\nexec sleep 30"));
        wait_until("the message", || run_get(&env, &id).is_ok_and(|run| run.last_output.is_some()));
        command(&env, "run.stop", &json!({ "run": id })).expect("stop");
        wait_until("the run to stop", || run_get(&env, &id).is_ok_and(|run| run.state == RunState::Stopped));
        let run = run_get(&env, &id).expect("run");
        assert_eq!(run.drafts.len(), 1, "the finished message is kept as an AI draft");
        assert_eq!(run.drafts[0].state, DraftState::Pending);
    }

    #[test]
    fn a_stopped_run_does_not_become_done_when_its_result_arrives_late() {
        let result = json!({ "type": "result", "subtype": "success", "is_error": false, "result": "[]" }).to_string();
        let (env, id) = running_engine("stop-late", &format!("cat >/dev/null\nsleep 1\necho '{result}'\nexec sleep 30"));
        command(&env, "run.stop", &json!({ "run": id })).expect("stop");
        wait_until("the run to end", || run_get(&env, &id).is_ok_and(|run| !run.state.is_active()));
        assert_eq!(run_get(&env, &id).expect("run").state, RunState::Stopped);
    }

    #[test]
    fn a_quiet_run_is_retried_once_and_then_flagged() {
        let root = root("stuck");
        let env = env_with(&root, fake_engine(&root, "exec sleep 30"));
        command(&env, "ai.engine.set", &json!({ "stuck": { "quiet_secs": 1, "long_step_secs": 1, "retry_once": true } })).expect("settings");
        new_agent(&env, &[]);
        let ids = start_in(&env, &source(DIFF), &detached()).expect("start");
        let id = &ids[0];
        wait_until("the retry", || run_get(&env, id).is_ok_and(|run| run.retries == 1));
        wait_until("the stuck flag", || run_get(&env, id).is_ok_and(|run| run.state == RunState::LooksStuck));
        assert_eq!(run_get(&env, id).expect("run").retries, 1, "it retries once, not again");
        command(&env, "run.stop_all", &Value::Null).expect("stop all");
        wait_until("the run to end", || run_get(&env, id).is_ok_and(|run| !run.state.is_active()));
    }

    #[test]
    fn the_retry_does_not_kill_a_run_that_is_streaming_output() {
        let tick = r#"{"type":"stream_event","event":{}}"#;
        let result = json!({ "type": "result", "subtype": "success", "is_error": false, "result": "[]" }).to_string();
        let script = format!("cat >/dev/null\nfor i in 1 2 3 4 5 6; do echo '{tick}'; sleep 0.5; done\necho '{result}'");
        let root = root("streaming");
        let env = env_with(&root, fake_engine(&root, &script));
        command(&env, "ai.engine.set", &json!({ "stuck": { "quiet_secs": 2, "long_step_secs": 2, "retry_once": true } })).expect("settings");
        new_agent(&env, &[]);
        let ids = start_in(&env, &source(DIFF), &waiting()).expect("start");
        let run = run_get(&env, &ids[0]).expect("run");
        assert_eq!((run.state, run.retries), (RunState::Done, 0), "partial messages keep the clock moving: {:?}", run.error);
    }

    /// A saved run that looks active, whose supervising process is long gone.
    fn orphan_run(env: &Env, pid: Option<(u32, Option<String>)>) -> Run {
        let part = Part {
            agent: RunAgent { id: None, name: "General review".into() },
            engine: Engine::ClaudeCode,
            template: String::new(),
            limits: SpawnLimits::default(),
            files: Vec::new(),
        };
        let prompt = Prompt { text: String::new(), sent: vec!["a.go".into()], skipped: Vec::new(), redactions: 0 };
        let mut run = new_run(&source(DIFF), &part, &prompt);
        run.state = RunState::Running;
        run.owner_pid = 4_000_000;
        run.owner_start = None;
        if let Some((pid, start)) = pid {
            run.pid = Some(pid);
            run.pid_start = start;
        }
        store::create_run(&env.dir, &run).expect("save");
        run
    }

    #[test]
    fn a_run_whose_supervisor_is_gone_is_marked_failed_not_left_running() {
        let root = root("orphan");
        let env = env_with(&root, PathBuf::from("unused"));
        orphan_run(&env, None);
        let listed = run_list(&env, Some(&change())).runs;
        assert_eq!(listed[0].state, RunState::Failed);
        assert!(listed[0].error.as_deref().is_some_and(|e| e.contains("stopped")));
        assert!(run_list(&env, Some(&ChangeRef { number: 9, ..change() })).runs.is_empty(), "the list filters by change");
    }

    #[test]
    fn an_orphaned_engine_is_killed_only_when_it_is_verified_to_be_ours() {
        let root = root("orphan-engine");
        let env = env_with(&root, PathBuf::from("unused"));
        let mut ours = std::process::Command::new("sleep").arg("30").spawn().expect("the engine the dead supervisor left");
        let mut bystander = std::process::Command::new("sleep").arg("30").spawn().expect("a process that reused the pid");
        let started = runner::process_started(ours.id());
        assert!(started.is_some());
        let mine = orphan_run(&env, Some((ours.id(), started)));
        let other = orphan_run(&env, Some((bystander.id(), Some("Thu Jan  1 00:00:00 1970".into()))));

        let listed = run_list(&env, None).runs;
        assert!(listed.iter().all(|run| run.state == RunState::Failed));
        assert!(ours.wait().is_ok_and(|status| !status.success()), "the verified engine was killed");
        assert!(bystander.try_wait().is_ok_and(|status| status.is_none()), "the process with another start time was left alone");
        let other = run_get(&env, &other.id).expect("run");
        assert!(other.tail.iter().any(|line| line.contains("left alone")), "and the run says so: {:?}", other.tail);
        assert!(run_get(&env, &mine.id).expect("run").tail.iter().any(|line| line.contains("Ended the engine")));
        drop(bystander.kill());
        drop(bystander.wait());
    }

    #[test]
    fn a_stale_read_never_overwrites_a_run_that_finished_meanwhile() {
        let root = root("stale");
        let env = env_with(&root, PathBuf::from("unused"));
        let mut stale = orphan_run(&env, None);
        // The supervisor finishes between the poll's read and its liveness check.
        let finished = store::with_run(&env.dir, &stale.id, |disk| {
            disk.state = RunState::Done;
            disk.drafts = vec![AiDraft {
                id: format!("{}/0", disk.id),
                finding: pr_craft_core::run::Finding {
                    path: "a.go".into(),
                    old_path: None,
                    line: pr_craft_core::review::LineRef { old: None, new: Some(2) },
                    body: "paid for".into(),
                    agent: "General review".into(),
                },
                state: DraftState::Pending,
            }];
        })
        .expect("save")
        .is_some();
        assert!(finished);
        reconcile(&env, &mut stale);
        assert_eq!(stale.state, RunState::Done, "the poll's copy is replaced by the fresh one");
        let saved = store::load_run(&env.dir, &stale.id).expect("load").expect("run");
        assert_eq!((saved.state, saved.drafts.len(), saved.error), (RunState::Done, 1, None), "the finished run and its drafts survive");
    }

    #[test]
    fn changes_to_one_run_from_two_writers_are_merged_not_overwritten() {
        let root = root("merge");
        let env = env_with(&root, PathBuf::from("unused"));
        let run = orphan_run(&env, None);
        let id = run.id.clone();
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        store::with_run(&env.dir, &id, |disk| disk.retries += 1).expect("update");
                    }
                });
            }
        });
        assert_eq!(store::load_run(&env.dir, &id).expect("load").expect("run").retries, 50, "no update was lost");

        let mut disk = run.clone();
        let mut local = run.clone();
        let keep = |state| AiDraft { id: "r/0".into(), finding: dummy_finding(), state };
        disk.drafts = vec![keep(DraftState::Kept)];
        local.state = RunState::Done;
        local.drafts = vec![keep(DraftState::Pending)];
        runner::merge_supervised(&mut disk, &local);
        assert_eq!(disk.state, RunState::Done, "the supervisor's state goes in");
        assert_eq!(disk.drafts[0].state, DraftState::Kept, "a keep made meanwhile is not undone");
        let mut ended = run;
        ended.state = RunState::Failed;
        runner::merge_supervised(&mut ended, &local);
        assert_eq!(ended.state, RunState::Failed, "a run settled by someone else stays as it is");
    }

    fn dummy_finding() -> pr_craft_core::run::Finding {
        pr_craft_core::run::Finding {
            path: "a.go".into(),
            old_path: None,
            line: pr_craft_core::review::LineRef { old: None, new: Some(2) },
            body: "b".into(),
            agent: "x".into(),
        }
    }

    #[test]
    fn the_plan_shows_who_reads_what_and_what_nobody_reads() {
        let root = root("plan");
        let env = env_with(&root, PathBuf::from("unused"));
        let diff = format!(
            "{DIFF}diff --git a/README.md b/README.md\nindex 1..2 100644\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/go.sum b/go.sum\nindex 1..2 100644\n--- a/go.sum\n+++ b/go.sum\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/logo.png b/logo.png\nindex 1..2 100644\nBinary files a/logo.png and b/logo.png differ\n"
        );
        let mut src = source(&diff);
        src.viewed.insert("a.go".into());
        let go = new_agent(&env, &["**/*.go"]);
        command(
            &env,
            "agent.create",
            &json!({ "name": "Elsewhere", "prompt": "x", "runs_on": { "kind": "repos", "repos": [{ "forge": "gitlab", "repo": "g/p" }] } }),
        )
        .expect("create");

        let plan = plan_in(&env, &src, &Options::default()).expect("plan");
        assert_eq!(plan.agents.len(), 1, "an agent bound to another repository is not in this plan");
        assert_eq!(plan.agents[0].files, vec!["a.go"]);
        assert_eq!(plan.general, vec!["README.md"], "the file no agent covers goes to the general pass");
        let skipped: Vec<_> = plan.skipped.iter().map(|s| (s.path.as_str(), s.reason.as_str())).collect();
        assert!(
            skipped.contains(&("deploy/.env", "matches a never-send pattern"))
                && skipped.contains(&("go.sum", "generated file"))
                && skipped.contains(&("logo.png", "binary file"))
        );
        assert!(plan.coverage.files.iter().all(|path| !skipped.iter().any(|(p, _)| p == path)), "nothing skipped counts as covered");

        let skipping = plan_in(&env, &src, &Options { skip_viewed: true, ..Options::default() }).expect("plan");
        assert!(skipping.agents[0].files.is_empty(), "a viewed file is left out when asked");
        assert_eq!(skipping.coverage.skipped_viewed, vec!["a.go"]);

        let only = plan_in(&env, &src, &Options { agents: Some(Vec::new()), ..Options::default() }).expect("plan");
        assert_eq!(only.general, vec!["a.go", "README.md"], "with the agent off its files go to the general pass");

        let strict = ReadRules { scope: ReadScope::WholeCheckout, ..ReadRules::default() };
        command(&env, "ai.read.set", &json!({ "forge": "github", "repo": "acme/api", "rules": strict })).expect("override");
        let blocked = plan_in(&env, &src, &Options::default()).expect("plan");
        assert!(blocked.blocked.is_some(), "a read scope that needs a checkout is not run as if it were diff only");
        assert!(start_in(&env, &src, &Options::default()).is_err());
        assert!(go.id.starts_with("agent-"));
    }

    #[test]
    fn a_review_with_nothing_to_read_does_not_start() {
        let root = root("empty");
        let env = env_with(&root, PathBuf::from("unused"));
        let only_env = source(
            "diff --git a/deploy/.env b/deploy/.env\nnew file mode 100644\nindex 0..1\n--- /dev/null\n+++ b/deploy/.env\n@@ -0,0 +1 @@\n+A=1\n",
        );
        let refused = start_in(&env, &only_env, &Options::default()).expect_err("nothing to read");
        assert!(refused.to_string().contains("nothing"), "{refused}");
        assert!(runs(&env).is_empty());
    }

    #[test]
    fn agents_are_created_changed_listed_and_deleted() {
        let root = root("agents");
        let env = env_with(&root, PathBuf::from("unused"));
        assert!(command(&env, "agent.create", &json!({ "name": "  ", "prompt": "x" })).is_err(), "a name is required");
        assert!(command(&env, "agent.create", &json!({ "name": "A", "prompt": " " })).is_err(), "a prompt is required");
        assert!(command(&env, "agent.create", &json!({ "name": "A", "prompt": "x", "colour": "red" })).is_err(), "an unknown field is refused");
        let agent = new_agent(&env, &["  **/*.go ", ""]);
        assert_eq!(agent.scope, vec!["**/*.go"]);
        assert_eq!(agent.triggers, vec![Trigger::Manual]);

        let changed = command(&env, "agent.update", &json!({ "id": agent.id, "name": "Go", "scope": ["db/**"], "runs_on": { "kind": "repos", "repos": [{ "forge": "github", "repo": "acme/api" }] } })).expect("update");
        assert_eq!(changed["name"], json!("Go"));
        assert_eq!(changed["prompt"], json!("Review Go changes."), "what the patch leaves out stays");
        let here = command(&env, "agent.list", &json!({ "forge": "github", "repo": "acme/api" })).expect("list");
        assert_eq!(here.as_array().map(Vec::len), Some(1));
        let elsewhere = command(&env, "agent.list", &json!({ "forge": "github", "repo": "acme/web" })).expect("list");
        assert_eq!(elsewhere.as_array().map(Vec::len), Some(0));
        assert!(command(&env, "agent.list", &json!({ "forge": "github" })).is_err());
        assert!(command(&env, "agent.update", &json!({ "id": "nope", "name": "x" })).is_err());

        assert_eq!(command(&env, "agent.delete", &json!({ "id": agent.id })).expect("delete")["deleted"], json!(true));
        assert_eq!(
            command(&env, "agent.delete", &json!({ "id": agent.id })).expect("again")["deleted"],
            json!(false),
            "deleting what is gone is not an error"
        );
        assert_eq!(command(&env, "agent.list", &Value::Null).expect("list").as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn engine_settings_are_validated_and_kept() {
        let root = root("settings");
        let env = env_with(&root, PathBuf::from("unused"));
        assert!(command(&env, "ai.engine.set", &json!({ "runs_at_once": 0 })).is_err());
        assert!(
            command(&env, "ai.engine.set", &json!({ "stuck": { "quiet_secs": 600, "long_step_secs": 60 } })).is_err(),
            "long steps get at least as long"
        );
        let set = command(&env, "ai.engine.set", &json!({ "runs_at_once": 2, "engine": "zai_glm" })).expect("set");
        assert_eq!(set["settings"]["runs_at_once"], json!(2));
        let got = command(&env, "ai.engine.get", &Value::Null).expect("get");
        assert_eq!(got["settings"]["engine"], json!("zai_glm"));
        assert_eq!(got["settings"]["stuck"]["retry_once"], json!(true), "what was not set keeps its default");
        assert_eq!(got["engines"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn command_results_read_back_into_their_own_types() {
        let root = root("typed");
        let env = env_with(&root, PathBuf::from("unused"));
        new_agent(&env, &["**/*.go"]);
        let plan = plan_in(&env, &source(DIFF), &Options::default()).expect("plan");
        let back: Plan = serde_json::from_value(serde_json::to_value(&plan).expect("encode")).expect("a Plan reads back");
        assert_eq!(back.coverage, plan.coverage);
        let view = command(&env, "ai.engine.get", &Value::Null).expect("get");
        serde_json::from_value::<EngineView>(view).expect("an EngineView reads back");
        let rows = vec![PreviewRow { change: change(), title: "t".into(), outcome: pr_craft_core::agent::Outcome::Skipped { reason: "r".into() } }];
        let again: Vec<PreviewRow> = serde_json::from_value(serde_json::to_value(&rows).expect("encode")).expect("rows read back");
        assert_eq!(again, rows);
        let listed = command(&env, "run.list", &Value::Null).expect("list");
        serde_json::from_value::<RunList>(listed).expect("a RunList reads back");
    }
}
