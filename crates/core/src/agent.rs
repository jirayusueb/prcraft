//! Review agents, the AI engine settings and which agent reads which file. No I/O.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::extra::Extra;
use crate::redact::ReadRules;
use crate::{ChangeRef, Forge, filter};

/// The engine that runs an agent's prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// The `claude` CLI with the login it already has.
    #[default]
    ClaudeCode,
    /// GLM models through the Z.ai API.
    ZaiGlm,
}

impl Engine {
    pub const ALL: [Engine; 2] = [Engine::ClaudeCode, Engine::ZaiGlm];

    pub fn name(self) -> &'static str {
        match self {
            Engine::ClaudeCode => "Claude Code",
            Engine::ZaiGlm => "Z.ai",
        }
    }

    /// Whether a run on this engine can finish. Z.ai needs a documented API call and a Keychain key that are not built yet.
    pub fn wired(self) -> bool {
        matches!(self, Engine::ClaudeCode)
    }
}

/// How many subagents a run may start beyond its plan. A spawn past a limit is refused and logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpawnLimits {
    /// Subagents started during the run, besides the planned ones.
    pub extra: u32,
    /// How deep a chain of spawns may go; a planned subagent is depth 0.
    pub depth: u32,
    /// Subagents running at the same moment.
    pub at_once: u32,
}

impl Default for SpawnLimits {
    fn default() -> Self {
        SpawnLimits { extra: 8, depth: 2, at_once: 4 }
    }
}

/// When a run with no output counts as stuck (PRODUCT.md: 5 minutes, 15 for steps known to be long).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StuckRules {
    pub quiet_secs: u64,
    pub long_step_secs: u64,
    /// Kill a stuck run and start it again once before telling the reviewer.
    pub retry_once: bool,
}

impl Default for StuckRules {
    fn default() -> Self {
        StuckRules { quiet_secs: 5 * 60, long_step_secs: 15 * 60, retry_once: true }
    }
}

/// Settings → AI engine, and the read rules of Settings → AI engine → what is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    /// The engine for the general pass and for new agents.
    pub engine: Engine,
    /// Runs going at once on this Mac; the rest wait as queued.
    pub runs_at_once: u32,
    pub stuck: StuckRules,
    /// The limits a new agent starts with.
    pub spawn: SpawnLimits,
    pub read: ReadRules,
    /// Per-repository read rules, by [`repo_key`]; they replace `read` whole for that repository.
    pub repo_read: BTreeMap<String, ReadRules>,
    /// Estimated input tokens per local day past which `ai.review.start` refuses; `None` is no ceiling.
    pub daily_token_ceiling: Option<u64>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for EngineSettings {
    fn default() -> Self {
        EngineSettings {
            engine: Engine::default(),
            runs_at_once: 4,
            stuck: StuckRules::default(),
            spawn: SpawnLimits::default(),
            read: ReadRules::default(),
            repo_read: BTreeMap::new(),
            daily_token_ceiling: None,
            extra: Extra::default(),
        }
    }
}

impl EngineSettings {
    /// The rules that apply to one repository's changes.
    pub fn read_for(&self, forge: Forge, repo: &str) -> &ReadRules {
        self.repo_read.get(&repo_key(forge, repo)).unwrap_or(&self.read)
    }
}

/// `gh:acme/api`: the key of a repository in per-repository settings. The forges ignore case in a repository path, so the key does too.
pub fn repo_key(forge: Forge, repo: &str) -> String {
    format!("{}:{}", forge.cli(), repo.to_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// The reviewer starts it with AI review.
    Manual,
    /// Starts when a review is requested of the user.
    ReviewRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRef {
    pub forge: Forge,
    pub repo: String,
}

/// Where an agent runs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunsOn {
    #[default]
    All,
    Repos {
        repos: Vec<RepoRef>,
    },
}

impl RunsOn {
    pub fn includes(&self, forge: Forge, repo: &str) -> bool {
        match self {
            RunsOn::All => true,
            RunsOn::Repos { repos } => repos.iter().any(|r| r.forge == forge && r.repo.eq_ignore_ascii_case(repo)),
        }
    }
}

/// A named reviewer: a prompt, the skills it may use and the files it reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// May use `{pr_title}`, `{files}`, `{diff}` and `{base_branch}`.
    pub prompt: String,
    /// Skill folders or files, as the user attached them.
    #[serde(default)]
    pub skills: Vec<String>,
    pub engine: Engine,
    /// Include and exclude patterns in [`filter::visible`] syntax (`**/*.go`, `!*_test.go`).
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default = "default_triggers")]
    pub triggers: Vec<Trigger>,
    #[serde(default)]
    pub spawn: SpawnLimits,
    #[serde(default)]
    pub runs_on: RunsOn,
    #[serde(flatten, default)]
    pub extra: Extra,
}

fn default_triggers() -> Vec<Trigger> {
    vec![Trigger::Manual]
}

impl Agent {
    pub fn covers(&self, path: &str) -> bool {
        filter::visible(&self.scope, path)
    }

    pub fn runs_on_repo(&self, forge: Forge, repo: &str) -> bool {
        self.runs_on.includes(forge, repo)
    }
}

/// The prompt of the general pass, for files no agent covers.
pub const GENERAL_PROMPT: &str = "You are a careful code reviewer. Report correctness bugs, security problems, missing error handling and risky changes in the diff. \
Skip style and naming remarks. For each finding, say what can go wrong and propose the smallest fix.";

/// One changed file as coverage sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverFile {
    pub path: String,
    /// The reviewer marked it viewed.
    pub viewed: bool,
}

/// The files × agents map of the AI review sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// The files that will be reviewed, in change order.
    pub files: Vec<String>,
    /// Left out because they are marked viewed.
    pub skipped_viewed: Vec<String>,
    /// The columns: agent ids.
    pub agents: Vec<String>,
    /// File path to the ids of the agents that cover it.
    pub cells: BTreeMap<String, Vec<String>>,
    /// Files no agent covers; the general pass reads them.
    pub general: Vec<String>,
}

impl Coverage {
    /// The files `agent` reads, in change order.
    pub fn files_of(&self, agent: &str) -> Vec<String> {
        self.files.iter().filter(|path| self.cells.get(*path).is_some_and(|ids| ids.iter().any(|id| id == agent))).cloned().collect()
    }
}

/// Which agent covers which file. `agents` are the ones that run on this repository. With `skip_viewed`, files
/// marked viewed are left out of every column and out of the general pass.
pub fn coverage(files: &[CoverFile], agents: &[Agent], skip_viewed: bool) -> Coverage {
    let (skipped, kept): (Vec<&CoverFile>, Vec<&CoverFile>) = files.iter().partition(|file| skip_viewed && file.viewed);
    let mut cells = BTreeMap::new();
    let mut general = Vec::new();
    for file in &kept {
        let ids: Vec<String> = agents.iter().filter(|agent| agent.covers(&file.path)).map(|agent| agent.id.clone()).collect();
        if ids.is_empty() {
            general.push(file.path.clone());
        }
        cells.insert(file.path.clone(), ids);
    }
    Coverage {
        files: kept.iter().map(|file| file.path.clone()).collect(),
        skipped_viewed: skipped.iter().map(|file| file.path.clone()).collect(),
        agents: agents.iter().map(|agent| agent.id.clone()).collect(),
        cells,
        general,
    }
}

/// A change that `agent.scope_preview` weighs an agent's scope against.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub change: ChangeRef,
    pub title: String,
    /// The reviewable files of the change, or why they could not be read.
    pub files: Result<Vec<String>, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Runs { files: Vec<String> },
    Skipped { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewRow {
    pub change: ChangeRef,
    pub title: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// For each change: whether an agent with this scope and place would run on it, and on which files.
pub fn scope_preview(scope: &[String], runs_on: &RunsOn, candidates: Vec<Candidate>) -> Vec<PreviewRow> {
    candidates
        .into_iter()
        .map(|Candidate { change, title, files }| {
            let outcome = if !runs_on.includes(change.forge, &change.repo) {
                Outcome::Skipped { reason: "the agent does not run on this repository".into() }
            } else {
                match files {
                    Err(error) => Outcome::Skipped { reason: format!("its files could not be read: {error}") },
                    Ok(files) => {
                        let files: Vec<String> = files.into_iter().filter(|path| filter::visible(scope, path)).collect();
                        if files.is_empty() {
                            Outcome::Skipped { reason: "no changed file matches the scope".into() }
                        } else {
                            Outcome::Runs { files }
                        }
                    }
                }
            };
            PreviewRow { change, title, outcome }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, scope: &[&str]) -> Agent {
        Agent {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            prompt: "Review.".into(),
            skills: Vec::new(),
            engine: Engine::ClaudeCode,
            scope: scope.iter().map(|p| (*p).to_string()).collect(),
            triggers: default_triggers(),
            spawn: SpawnLimits::default(),
            runs_on: RunsOn::All,
            extra: Extra::default(),
        }
    }

    fn file(path: &str, viewed: bool) -> CoverFile {
        CoverFile { path: path.into(), viewed }
    }

    #[test]
    fn a_file_no_agent_covers_goes_to_the_general_pass() {
        let agents = [agent("go", &["**/*.go", "!*_test.go"]), agent("sql", &["db/**"])];
        let files = [file("internal/a.go", false), file("internal/a_test.go", false), file("db/0087.sql", false), file("README.md", false)];
        let map = coverage(&files, &agents, false);
        assert_eq!(map.cells["internal/a.go"], vec!["go"]);
        assert_eq!(map.cells["db/0087.sql"], vec!["sql"]);
        assert_eq!(map.general, vec!["internal/a_test.go", "README.md"], "excluded and unmatched files are not dropped");
        assert_eq!(map.files_of("go"), vec!["internal/a.go"]);
        assert_eq!(map.files.len(), 4);
    }

    #[test]
    fn two_agents_can_cover_one_file() {
        let agents = [agent("a", &[]), agent("b", &["*.go"])];
        let map = coverage(&[file("x.go", false)], &agents, false);
        assert_eq!(map.cells["x.go"], vec!["a", "b"]);
        assert!(map.general.is_empty(), "an agent with no pattern covers every file");
    }

    #[test]
    fn viewed_files_are_skipped_only_when_asked() {
        let files = [file("a.go", true), file("b.go", false)];
        let agents = [agent("go", &["*.go"])];
        let skipping = coverage(&files, &agents, true);
        assert_eq!(skipping.files, vec!["b.go"]);
        assert_eq!(skipping.skipped_viewed, vec!["a.go"]);
        assert!(!skipping.cells.contains_key("a.go"));
        let all = coverage(&files, &agents, false);
        assert_eq!(all.files.len(), 2);
        assert!(all.skipped_viewed.is_empty());
    }

    #[test]
    fn scope_preview_says_why_a_change_is_skipped() {
        let change = |repo: &str, number| ChangeRef { forge: Forge::GitHub, repo: repo.into(), number, host: None };
        let candidate = |repo: &str, number, files: Result<Vec<&str>, &str>| Candidate {
            change: change(repo, number),
            title: String::new(),
            files: files.map(|f| f.into_iter().map(String::from).collect()).map_err(String::from),
        };
        let runs_on = RunsOn::Repos { repos: vec![RepoRef { forge: Forge::GitHub, repo: "acme/api".into() }] };
        let rows = scope_preview(
            &["**/*.go".to_string()],
            &runs_on,
            vec![
                candidate("acme/api", 1, Ok(vec!["a/b.go", "README.md"])),
                candidate("acme/api", 2, Ok(vec!["README.md"])),
                candidate("acme/api", 3, Err("offline")),
                candidate("acme/web", 4, Ok(vec!["x.go"])),
            ],
        );
        assert_eq!(rows[0].outcome, Outcome::Runs { files: vec!["a/b.go".into()] });
        assert!(matches!(&rows[1].outcome, Outcome::Skipped { reason } if reason.contains("no changed file")));
        assert!(matches!(&rows[2].outcome, Outcome::Skipped { reason } if reason.contains("offline")));
        assert!(matches!(&rows[3].outcome, Outcome::Skipped { reason } if reason.contains("repository")));
    }

    #[test]
    fn a_repository_override_replaces_the_global_read_rules() {
        let mut settings = EngineSettings::default();
        let mut strict = ReadRules::default();
        strict.never_send.push("billing/**".into());
        settings.repo_read.insert(repo_key(Forge::GitLab, "acme/billing"), strict.clone());
        assert_eq!(settings.read_for(Forge::GitLab, "acme/billing"), &strict);
        assert_eq!(settings.read_for(Forge::GitHub, "acme/billing"), &settings.read, "same path on the other forge is another repo");
        assert_eq!(settings.read_for(Forge::GitLab, "Acme/Billing"), &strict, "a repository path is the same in any case, so is its override");
        let place = RunsOn::Repos { repos: vec![RepoRef { forge: Forge::GitLab, repo: "acme/billing".into() }] };
        assert!(place.includes(Forge::GitLab, "ACME/Billing"));
    }
}
