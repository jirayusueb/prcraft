//! An AI run: one agent on one change, its subagent tree, and the findings it produced. No I/O.
//!
//! Findings are AI drafts. They become a normal draft only through `draft.keep`, and post only through `review.submit`.

use serde::{Deserialize, Serialize};

use crate::agent::{Engine, SpawnLimits, StuckRules};
use crate::extra::Extra;
use crate::review::{Anchor, LineRef};
use crate::{ChangeRef, DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Waiting for a free slot of "runs at once".
    Queued,
    Running,
    /// No output for longer than the stuck threshold. Output makes it `Running` again.
    LooksStuck,
    Done,
    Failed,
    /// Ended by Stop, after the step it was on.
    Stopped,
    /// Ended by Kill: the process was killed.
    Killed,
}

impl RunState {
    pub fn is_active(self) -> bool {
        matches!(self, RunState::Queued | RunState::Running | RunState::LooksStuck)
    }
}

/// The agent a run belongs to; `id` is `None` for the general pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAgent {
    pub id: Option<String>,
    pub name: String,
}

/// A file the run did not send, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

/// A problem an agent found, on a line of the diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub path: String,
    /// The path before a rename, so GitLab can anchor the comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub line: LineRef,
    pub body: String,
    /// The name of the agent that found it.
    pub agent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftState {
    /// Shown on the diff, not part of the review yet.
    Pending,
    /// Became a draft of the review (`draft.keep`).
    Kept,
    Discarded,
}

/// A finding waiting for the reviewer to keep or discard it. It posts nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiDraft {
    /// `<run id>/<number>`; `draft.keep` and `draft.discard` take it.
    pub id: String,
    pub finding: Finding,
    pub state: DraftState,
}

/// Who started a subagent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpawnedBy {
    Skill { name: String },
    Subagent { node: u32 },
    Runner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Queued,
    Running,
    Done,
    Failed,
    Skipped,
    Stopped,
    /// A spawn past a limit; see [`Node::refusal`].
    Refused,
}

/// Why a spawn was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "limit", rename_all = "snake_case")]
pub enum Refusal {
    Extra { max: u32 },
    Depth { max: u32 },
    AtOnce { max: u32 },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Extra { max } => write!(f, "extra subagent limit {max} reached"),
            Refusal::Depth { max } => write!(f, "depth limit {max}"),
            Refusal::AtOnce { max } => write!(f, "{max} subagents already running at once"),
        }
    }
}

/// One subagent of a run, or the run's own work (the root).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: u32,
    pub parent: Option<u32>,
    /// A planned node is depth 0; a spawned one is one deeper than its parent.
    pub depth: u32,
    /// In the plan from the start, rather than spawned during the run.
    pub planned: bool,
    pub spawned_by: Option<SpawnedBy>,
    /// The rule that made it spawn, when it was spawned.
    pub reason: Option<String>,
    pub task: String,
    pub status: NodeStatus,
    pub started: Option<DateTime<Utc>>,
    pub ended: Option<DateTime<Utc>>,
    pub files_read: Vec<String>,
    pub findings: u32,
    pub refusal: Option<Refusal>,
}

/// One run of one agent on one change; persisted so the CLI and the Activity list see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub change: ChangeRef,
    pub change_title: String,
    pub agent: RunAgent,
    pub engine: Engine,
    pub state: RunState,
    /// When it left the queue and began.
    pub started: DateTime<Utc>,
    pub ended: Option<DateTime<Utc>>,
    /// The last time the engine printed anything.
    pub last_output: Option<DateTime<Utc>>,
    /// The engine process; `None` before it starts.
    pub pid: Option<u32>,
    /// When the engine process started, read right after it spawned. A process is signalled by pid only while it still
    /// shows this start time, so a reused pid is never killed. `None` when unreadable: then it is never signalled by pid.
    #[serde(default)]
    pub pid_start: Option<String>,
    /// The PR Craft process that supervises the run. A run whose owner is gone is marked failed.
    pub owner_pid: u32,
    /// When the supervising process started (`ps -o lstart=`), so a reused pid is not mistaken for it. `None` when unreadable.
    #[serde(default)]
    pub owner_start: Option<String>,
    /// The commit the diff belongs to; a kept draft anchors to it.
    pub anchor: Anchor,
    /// The files the prompt carried.
    pub files: Vec<String>,
    pub skipped: Vec<Skipped>,
    /// Secrets masked in the prompt.
    pub redactions: u32,
    pub limits: SpawnLimits,
    /// The subagent tree; node 0 is the run itself.
    pub tree: Vec<Node>,
    pub drafts: Vec<AiDraft>,
    /// Findings the engine gave that name no changed file or no diff line; they are never guessed at.
    pub dropped: u32,
    /// The engine is on a step known to be long (tests, builds, rebases), so quiet lasts longer.
    pub long_step: bool,
    /// Stop was asked for; the run ends after its current step.
    pub stopping: bool,
    /// Automatic retries used after a stuck run.
    pub retries: u32,
    pub error: Option<String>,
    /// The last lines the engine printed, oldest first.
    pub tail: Vec<String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// How many output lines a run keeps for the Activity window.
pub const TAIL_LINES: usize = 20;

impl Run {
    /// Keeps the last [`TAIL_LINES`] lines of output.
    pub fn push_tail(&mut self, line: String) {
        self.tail.push(line);
        if self.tail.len() > TAIL_LINES {
            self.tail.remove(0);
        }
    }

    /// Starts a subagent under `parent`, unless a limit refuses it. A refusal is logged as a refused node of the tree.
    pub fn spawn(&mut self, parent: u32, by: SpawnedBy, reason: &str, task: &str, now: DateTime<Utc>) -> Result<u32, Refusal> {
        let verdict = may_spawn(&self.tree, parent, &self.limits);
        let depth = self.tree.iter().find(|node| node.id == parent).map_or(0, |node| node.depth + 1);
        let id = self.tree.iter().map(|node| node.id + 1).max().unwrap_or(0);
        let (status, refusal) = match verdict {
            Ok(()) => (NodeStatus::Running, None),
            Err(refusal) => (NodeStatus::Refused, Some(refusal)),
        };
        self.tree.push(Node {
            id,
            parent: Some(parent),
            depth,
            planned: false,
            spawned_by: Some(by),
            reason: Some(reason.to_string()),
            task: task.to_string(),
            status,
            started: refusal.is_none().then_some(now),
            ended: None,
            files_read: Vec::new(),
            findings: 0,
            refusal,
        });
        match refusal {
            None => Ok(id),
            Some(refusal) => {
                self.push_tail(format!("Refused to start \"{task}\": {refusal}"));
                Err(refusal)
            }
        }
    }
}

/// Whether a subagent may start under `parent`. Refused nodes do not count against any limit.
pub fn may_spawn(tree: &[Node], parent: u32, limits: &SpawnLimits) -> Result<(), Refusal> {
    let depth = tree.iter().find(|node| node.id == parent).map_or(0, |node| node.depth + 1);
    let extra = tree.iter().filter(|node| !node.planned && node.status != NodeStatus::Refused).count();
    let running = tree.iter().filter(|node| node.parent.is_some() && node.status == NodeStatus::Running).count();
    let over = |count: usize, max: u32| u32::try_from(count).map_or(true, |count| count >= max);
    if depth > limits.depth {
        Err(Refusal::Depth { max: limits.depth })
    } else if over(extra, limits.extra) {
        Err(Refusal::Extra { max: limits.extra })
    } else if over(running, limits.at_once) {
        Err(Refusal::AtOnce { max: limits.at_once })
    } else {
        Ok(())
    }
}

/// Whether the run has been quiet for longer than `rules` allow. Only a running run can look stuck.
pub fn stuck(run: &Run, now: DateTime<Utc>, rules: &StuckRules) -> bool {
    if !matches!(run.state, RunState::Running | RunState::LooksStuck) {
        return false;
    }
    let limit = if run.long_step { rules.long_step_secs } else { rules.quiet_secs };
    let quiet = now.signed_duration_since(run.last_output.unwrap_or(run.started)).num_seconds();
    quiet > i64::try_from(limit).unwrap_or(i64::MAX)
}

/// Whether a shell command is one of the steps that print little for a long while.
pub fn long_step_command(command: &str) -> bool {
    const WORDS: [&str; 8] = ["test", "build", "rebase", "compile", "install", "check", "lint", "migrate"];
    command.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| WORDS.contains(&word))
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::Forge;

    fn secs(n: u64) -> Duration {
        Duration::seconds(i64::try_from(n).unwrap_or(i64::MAX))
    }

    fn node(id: u32, parent: Option<u32>, depth: u32, planned: bool, status: NodeStatus) -> Node {
        Node {
            id,
            parent,
            depth,
            planned,
            spawned_by: None,
            reason: None,
            task: String::new(),
            status,
            started: None,
            ended: None,
            files_read: Vec::new(),
            findings: 0,
            refusal: None,
        }
    }

    fn run(now: DateTime<Utc>) -> Run {
        Run {
            id: "run-1".into(),
            change: ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 482, host: None },
            change_title: "Retry webhooks".into(),
            agent: RunAgent { id: Some("a1".into()), name: "Concurrency".into() },
            engine: Engine::ClaudeCode,
            state: RunState::Running,
            started: now,
            ended: None,
            last_output: None,
            pid: None,
            pid_start: None,
            owner_pid: 1,
            owner_start: None,
            anchor: Anchor { head_sha: "h1".into(), base_sha: None, start_sha: None },
            files: Vec::new(),
            skipped: Vec::new(),
            redactions: 0,
            limits: SpawnLimits { extra: 3, depth: 2, at_once: 2 },
            tree: vec![node(0, None, 0, true, NodeStatus::Running)],
            drafts: Vec::new(),
            dropped: 0,
            long_step: false,
            stopping: false,
            retries: 0,
            error: None,
            tail: Vec::new(),
            extra: Extra::default(),
        }
    }

    #[test]
    fn quiet_past_the_threshold_looks_stuck_and_output_resets_it() {
        let rules = StuckRules::default();
        let start = Utc::now();
        let mut run = run(start);
        assert!(!stuck(&run, start + secs(rules.quiet_secs) - Duration::seconds(1), &rules));
        let late = start + secs(rules.quiet_secs) + Duration::seconds(1);
        assert!(stuck(&run, late, &rules), "no output since it started");
        run.last_output = Some(late - Duration::seconds(5));
        assert!(!stuck(&run, late, &rules), "recent output");
    }

    #[test]
    fn a_long_step_is_given_longer() {
        let rules = StuckRules::default();
        let start = Utc::now();
        let mut run = run(start);
        let between = start + secs(rules.quiet_secs) + Duration::seconds(60);
        assert!(stuck(&run, between, &rules));
        run.long_step = true;
        assert!(!stuck(&run, between, &rules), "tests and builds may be quiet");
        assert!(stuck(&run, start + secs(rules.long_step_secs) + Duration::seconds(1), &rules));
    }

    #[test]
    fn only_a_running_run_can_look_stuck() {
        let rules = StuckRules::default();
        let start = Utc::now();
        let late = start + Duration::hours(3);
        for state in [RunState::Queued, RunState::Done, RunState::Failed, RunState::Stopped, RunState::Killed] {
            let mut run = run(start);
            run.state = state;
            assert!(!stuck(&run, late, &rules), "{state:?}");
        }
    }

    #[test]
    fn test_and_build_commands_are_long_steps() {
        assert!(long_step_command("go test -race ./internal/webhooks"));
        assert!(long_step_command("cargo build --release"));
        assert!(!long_step_command("ls -la"));
        assert!(!long_step_command("cat attestation.txt"), "a word that merely contains `test` is not a test run");
    }

    #[test]
    fn a_spawn_past_a_limit_is_refused_and_logged_on_the_run() {
        let now = Utc::now();
        let mut run = run(now);
        let skill = || SpawnedBy::Skill { name: "go-concurrency".into() };
        let first = run.spawn(0, skill(), "trace lock order", "lock-order", now).expect("within limits");
        let second = run.spawn(first, SpawnedBy::Subagent { node: first }, "check leaks", "goroutine-leak", now).expect("depth 2");
        let refused = run.spawn(second, SpawnedBy::Subagent { node: second }, "deadlocks", "deadlock-check", now);
        assert_eq!(refused, Err(Refusal::Depth { max: 2 }), "depth 3 is past the limit");
        let logged = run.tree.last().expect("the refusal is a node");
        assert_eq!(logged.status, NodeStatus::Refused);
        assert_eq!(logged.refusal, Some(Refusal::Depth { max: 2 }));
        assert!(run.tail.last().is_some_and(|line| line.contains("deadlock-check")), "and a line in the run's log");
        assert_eq!(run.spawn(0, skill(), "more", "third-extra", now), Err(Refusal::AtOnce { max: 2 }), "two are already running");
        assert!(run.tree.iter().filter(|n| n.refusal.is_some()).all(|n| n.status == NodeStatus::Refused));
    }

    #[test]
    fn the_extra_limit_counts_spawned_nodes_not_planned_or_refused_ones() {
        let limits = SpawnLimits { extra: 2, depth: 5, at_once: 10 };
        let mut tree = vec![node(0, None, 0, true, NodeStatus::Done), node(1, Some(0), 0, true, NodeStatus::Done)];
        assert_eq!(may_spawn(&tree, 0, &limits), Ok(()), "planned nodes are free");
        tree.push(node(2, Some(0), 1, false, NodeStatus::Done));
        tree.push(node(3, Some(0), 1, false, NodeStatus::Refused));
        assert_eq!(may_spawn(&tree, 0, &limits), Ok(()), "a refused spawn used no slot");
        tree.push(node(4, Some(0), 1, false, NodeStatus::Done));
        assert_eq!(may_spawn(&tree, 0, &limits), Err(Refusal::Extra { max: 2 }));
    }

    #[test]
    fn a_run_keeps_keys_a_newer_build_wrote() {
        let mut file = serde_json::to_value(run(Utc::now())).expect("encodes");
        file["from_the_future"] = serde_json::json!("kept");
        let loaded: Run = serde_json::from_value(file).expect("loads");
        assert_eq!(serde_json::to_value(&loaded).expect("saves")["from_the_future"], "kept");
    }
}
