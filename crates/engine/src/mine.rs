//! Your changes: the open changes the user wrote (`mine.list`, `mine.detail`), their checks (`checks.list`, `checks.log`,
//! `checks.rerun`), making a draft ready and its reviewers and labels (`pr.ready`, `pr.set_reviewers`, `pr.set_labels` and the reads
//! behind their pickers, `pr.collaborators` and `pr.labels`) and merging (`merge.start`, `merge.queue`, `merge.auto`).
//!
//! Merging cannot be undone and a re-run spends the user's CI, so `merge.*` and `checks.rerun` refuse without `confirm: true`, before
//! any call to the forge. Babysit and the AI conflict fix are not built, so there is no command for them.

use pr_craft_core::mine::{Check, Checks, MergeMethod, MergeState, Mine, MineDetail};
use pr_craft_core::{ChangeRef, Forge};
use pr_craft_forge::act::{self, Label, MergeCall, Page, Person};
use pr_craft_forge::{ForgeError, Login, UNKNOWN_OUTCOME, mine as read, outcome_unknown};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::{CommandError, HostAnswer};

/// One forge's Your changes.
#[derive(Debug, Clone, Serialize)]
pub struct MineSource {
    pub forge: Forge,
    pub cli: &'static str,
    #[serde(flatten)]
    pub state: MineState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MineState {
    /// At least one signed-in host answered. A host that failed is in `hosts` with its error, so the list is partial.
    Listed {
        /// Newest update first.
        changes: Vec<Mine>,
        /// Some host held more than one page, so older changes may be missing.
        truncated: bool,
        hosts: Vec<HostAnswer>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        signed_out: Vec<String>,
    },
    Paused {
        login: Login,
    },
    Failed {
        error: String,
    },
}

impl MineState {
    /// Whether this answer leaves changes out, so an empty list is not an all-clear.
    pub fn incomplete(&self) -> bool {
        match self {
            MineState::Listed { hosts, .. } => hosts.iter().any(|h| h.error.is_some()),
            MineState::Paused { .. } | MineState::Failed { .. } => true,
        }
    }
}

/// `mine.list`: both forges at once.
pub fn list() -> Vec<MineSource> {
    std::thread::scope(|scope| {
        let running: Vec<_> = Forge::ALL.map(|forge| scope.spawn(move || source(forge))).into();
        running.into_iter().zip(Forge::ALL).map(|(job, forge)| job.join().unwrap_or_else(|_| stopped(forge))).collect()
    })
}

fn stopped(forge: Forge) -> MineSource {
    MineSource { forge, cli: forge.cli(), state: MineState::Failed { error: "the sync thread stopped".into() } }
}

/// What a read of one forge found, and when: Your changes and Back to you read the same lists, so one read serves both.
struct Held {
    at: Instant,
    generation: u64,
    source: MineSource,
}

static GITHUB: Mutex<Option<Held>> = Mutex::new(None);
static GITLAB: Mutex<Option<Held>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// How long a read answers for the next one.
const FRESH: Duration = Duration::from_secs(30);

/// Throws the held reads away, so the next one asks the forges. Call it after a write: what it changed must show.
pub fn invalidate() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Drops the held reads when a write command ends, whether it worked, was refused or ran out of time: any of them may have changed
/// the forge, and a refusal costs only one read.
struct Stale;

impl Drop for Stale {
    fn drop(&mut self) {
        invalidate();
    }
}

/// The answer in `slot` while it is under [`FRESH`] old and no [`invalidate`] on `generation` came since it was started; otherwise
/// `read`, held only when it is a listing. Two calls that start together make one `read`. A `read` that was under way when a write
/// invalidated is returned to its caller but never served to the next one: it may predate the write.
fn held_read(slot: &Mutex<Option<Held>>, generation: &AtomicU64, read: impl FnOnce() -> MineSource) -> MineSource {
    let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
    let started = generation.load(Ordering::SeqCst);
    if let Some(found) = held.as_ref().filter(|h| h.generation == started && h.at.elapsed() < FRESH) {
        return found.source.clone();
    }
    let source = read();
    *held = matches!(source.state, MineState::Listed { .. }).then(|| Held { at: Instant::now(), generation: started, source: source.clone() });
    source
}

/// One forge's Your changes, read from every host its CLI is signed in to, all hosts at once. Blocks on the CLI; the UI calls it per
/// forge so rows appear as each source answers. A read made in the last 30 seconds answers instead, unless [`invalidate`] came since:
/// a re-check, a poll that follows another, and a write all behave as they say.
pub fn source(forge: Forge) -> MineSource {
    let slot = match forge {
        Forge::GitHub => &GITHUB,
        Forge::GitLab => &GITLAB,
    };
    held_read(slot, &GENERATION, || read_source(forge))
}

fn read_source(forge: Forge) -> MineSource {
    let state = match pr_craft_forge::hosts(forge) {
        Err(login) => MineState::Paused { login },
        Ok(hosts) => {
            let (signed, out): (Vec<_>, Vec<_>) = hosts.into_iter().partition(|h| h.signed_in);
            let signed_out = out.into_iter().map(|h| h.name).collect();
            if signed.is_empty() {
                MineState::Paused { login: pr_craft_forge::signed_out(forge) }
            } else {
                let answers: Vec<_> = std::thread::scope(|scope| {
                    let jobs: Vec<_> = signed.iter().map(|h| (h.name.clone(), scope.spawn(move || read::mine_on(forge, &h.name)))).collect();
                    jobs.into_iter().map(|(host, job)| (host, job.join().unwrap_or_else(|_| Err("the read stopped".into())))).collect()
                });
                joined(answers, signed_out)
            }
        }
    };
    MineSource { forge, cli: forge.cli(), state }
}

type HostRead = (String, Result<(Vec<Mine>, bool), pr_craft_forge::ForgeError>);

/// Joins per-host answers: Listed while any host answered (failures stay in `hosts`), Failed when none did.
fn joined(answers: Vec<HostRead>, signed_out: Vec<String>) -> MineState {
    if answers.iter().all(|(_, a)| a.is_err()) {
        let error = answers.iter().filter_map(|(host, a)| a.as_ref().err().map(|e| format!("{host}: {e}"))).collect::<Vec<_>>().join("; ");
        return MineState::Failed { error };
    }
    let (mut changes, mut hosts, mut truncated) = (Vec::new(), Vec::new(), false);
    for (host, answer) in answers {
        match answer {
            Ok((found, cut)) => {
                hosts.push(HostAnswer { host, listed: found.len(), truncated: cut, error: None });
                truncated |= cut;
                changes.extend(found);
            }
            Err(error) => hosts.push(HostAnswer { host, listed: 0, truncated: false, error: Some(error.message) }),
        }
    }
    changes.sort_by_key(|m| std::cmp::Reverse(m.change.updated_at));
    MineState::Listed { changes, truncated, hosts, signed_out }
}

/// The own changes a reviewer waits on, for Back to you: whether the read was partial is said in the second part.
pub fn waiting_on_you(forge: Forge) -> (Vec<Mine>, Option<String>) {
    match source(forge).state {
        MineState::Listed { changes, truncated, hosts, .. } => {
            let failed: Vec<String> = hosts.iter().filter_map(|h| h.error.as_ref().map(|e| format!("{}: {e}", h.host))).collect();
            let mut notes = Vec::new();
            if !failed.is_empty() {
                notes.push(format!("Could not read your own changes on {}, so reviewers waiting on you there are missing.", failed.join("; ")));
            }
            if truncated {
                notes.push(format!("{} returned its limit of {} of your changes, so older ones may be missing.", forge.cli(), pr_craft_forge::PAGE));
            }
            (changes.into_iter().filter(Mine::waiting_on_you).collect(), (!notes.is_empty()).then(|| notes.join(" ")))
        }
        MineState::Paused { .. } => (Vec::new(), None),
        MineState::Failed { error } => (Vec::new(), Some(format!("Could not read your own changes: {error}"))),
    }
}

#[derive(Deserialize)]
struct On {
    change: ChangeRef,
}

/// `mine.detail`: one change in full. Blocks on the CLI.
pub fn detail(change: &ChangeRef) -> Result<MineDetail, CommandError> {
    read::detail_on(change).map_err(CommandError::Forge)
}

/// `checks.list`.
#[derive(Debug, Clone, Serialize)]
pub struct ChecksList {
    pub change: ChangeRef,
    pub summary: Checks,
    pub checks: Vec<Check>,
}

pub fn checks_list(change: &ChangeRef) -> Result<ChecksList, CommandError> {
    let checks = read::checks_on(change).map_err(CommandError::Forge)?;
    Ok(ChecksList { change: change.clone(), summary: Checks::of(&checks), checks })
}

#[derive(Deserialize)]
struct LogOf {
    change: ChangeRef,
    check: String,
    /// The whole job instead of its failed steps (GitHub).
    #[serde(default)]
    all: bool,
}

/// `checks.log`.
#[derive(Debug, Clone, Serialize)]
pub struct LogResult {
    pub change: ChangeRef,
    pub check: String,
    #[serde(flatten)]
    pub log: pr_craft_core::mine::JobLog,
}

pub fn checks_log(change: &ChangeRef, check: &str, all: bool) -> Result<LogResult, CommandError> {
    let log = read::log_on(change, check, all).map_err(CommandError::Forge)?;
    Ok(LogResult { change: change.clone(), check: check.to_string(), log })
}

/// What a write did, for the screen to show.
#[derive(Debug, Clone, Serialize)]
pub struct Done {
    pub change: ChangeRef,
    pub message: String,
}

fn done(change: &ChangeRef, message: String) -> Result<Done, CommandError> {
    Ok(Done { change: change.clone(), message })
}

fn refuse_without_confirm(confirm: bool, what: &str) -> Result<(), CommandError> {
    if confirm {
        Ok(())
    } else {
        Err(CommandError::Invalid(format!("{what} needs `confirm: true`: say yes only after the reader has seen what it will do.")))
    }
}

#[derive(Deserialize)]
struct RerunOf {
    change: ChangeRef,
    check: String,
    #[serde(default)]
    confirm: bool,
}

/// `checks.rerun`: runs one job again on the forge.
pub fn checks_rerun(change: &ChangeRef, check: &str, confirm: bool) -> Result<Done, CommandError> {
    let _stale = Stale;
    refuse_without_confirm(confirm, "checks.rerun")?;
    act::rerun(change, check).map_err(|e| CommandError::Invalid(e.message))?.send().map_err(CommandError::Forge)?;
    done(change, format!("Asked {} to run the job again. It starts over on the forge; its log is empty until it finishes.", change.forge.name()))
}

/// `pr.ready`: takes a draft out of draft, then reads the change back: a forge that answered and left it a draft is an error.
pub fn pr_ready(change: &ChangeRef) -> Result<Done, CommandError> {
    let _stale = Stale;
    act::ready(change).send().map_err(CommandError::Forge)?;
    let now = read::detail_on(change)
        .map_err(|e| ForgeError::new(e.kind, format!("It was sent, but the change could not be read back to check it: {e}")))?;
    verify_ready(&now.mine)?;
    done(change, format!("{} is ready for review. The reviewers you asked are notified now.", change.reference()))
}

/// A draft the forge still lists as a draft was not made ready, whatever its CLI answered.
fn verify_ready(mine: &Mine) -> Result<(), CommandError> {
    if mine.change.draft {
        return Err(CommandError::Forge(ForgeError::from(format!(
            "The request was sent, but {} still lists {} as a draft. It was not made ready.",
            mine.change.forge.name(),
            mine.change.reference()
        ))));
    }
    Ok(())
}

/// Asks `add` to review and takes `remove` off, against who is asked right now, so someone added since the reader looked stays.
pub fn pr_change_reviewers(change: &ChangeRef, add: &[String], remove: &[String]) -> Result<Done, CommandError> {
    let _stale = Stale;
    let current = act::requested_now(change).map_err(CommandError::Forge)?;
    let same = |a: &String, b: &String| a.eq_ignore_ascii_case(b);
    let mut want = current.clone();
    for who in add {
        if !want.iter().any(|w| same(w, who)) {
            want.push(who.clone());
        }
    }
    want.retain(|w| !remove.iter().any(|r| same(w, r)));
    let changed = act::set_reviewers(change, &current, &want).map_err(CommandError::Forge)?;
    let message = match changed {
        0 => "The reviewers were already these.".to_string(),
        n => format!("Reviewers set: {n} change{} on {}.", if n == 1 { "" } else { "s" }, change.reference()),
    };
    done(change, message)
}

#[derive(Deserialize)]
struct ReviewersOf {
    change: ChangeRef,
    reviewers: Vec<String>,
}

/// `pr.set_reviewers`: makes `reviewers` the people asked to review. Someone who already reviewed stays as a reviewer.
pub fn pr_set_reviewers(change: &ChangeRef, reviewers: &[String]) -> Result<Done, CommandError> {
    let _stale = Stale;
    let current = act::requested_now(change).map_err(CommandError::Forge)?;
    let changed = act::set_reviewers(change, &current, reviewers).map_err(CommandError::Forge)?;
    let message = match changed {
        0 => "The reviewers were already these.".to_string(),
        n => format!("Reviewers set: {n} change{} on {}.", if n == 1 { "" } else { "s" }, change.reference()),
    };
    done(change, message)
}

#[derive(Deserialize)]
struct LabelsOf {
    change: ChangeRef,
    labels: Vec<String>,
}

/// `pr.set_labels`: replaces the labels of the change with `labels`.
pub fn pr_set_labels(change: &ChangeRef, labels: &[String]) -> Result<Done, CommandError> {
    let _stale = Stale;
    act::set_labels(change, labels).map_err(|e| CommandError::Invalid(e.message))?.send().map_err(CommandError::Forge)?;
    done(change, format!("Labels of {} are now: {}.", change.reference(), if labels.is_empty() { "none".to_string() } else { labels.join(", ") }))
}

/// Adds `add` to the labels and takes `remove` off, whatever the labels are now: a label added since the reader looked stays.
pub fn pr_change_labels(change: &ChangeRef, add: &[String], remove: &[String]) -> Result<Done, CommandError> {
    let _stale = Stale;
    for write in act::change_labels(change, add, remove).map_err(|e| CommandError::Invalid(e.message))? {
        write.send().map_err(CommandError::Forge)?;
    }
    done(change, format!("Labels of {} changed: {} added, {} removed.", change.reference(), add.len(), remove.len()))
}

/// `pr.collaborators`: who the change can ask to review, one page.
pub fn pr_collaborators(change: &ChangeRef) -> Result<Page<Person>, CommandError> {
    act::collaborators(change).map_err(CommandError::Forge)
}

/// `pr.labels`: the repository's labels, one page.
pub fn pr_labels(change: &ChangeRef) -> Result<Page<Label>, CommandError> {
    act::labels_of(change).map_err(CommandError::Forge)
}

/// What `merge.start`, `merge.queue` and `merge.auto` take.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MergeAsk {
    /// The head commit the reader saw. Required to merge, queue or turn Merge when ready on: those are refused without it. Turning
    /// Merge when ready off merges nothing and needs none.
    #[serde(default)]
    pub head_sha: String,
    pub change: ChangeRef,
    /// Absent: the first method the repository allows.
    #[serde(default)]
    pub method: Option<MergeMethod>,
    /// The commit message; empty keeps the forge's own.
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub delete_branch: bool,
    /// `merge.auto` only: false turns Merge when ready off.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Must be true: the reader has seen the method and the branch it names.
    #[serde(default)]
    pub confirm: bool,
}

fn yes() -> bool {
    true
}

/// Which merge command is running, for what its refusals and results say.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    Start,
    Queue,
    Auto,
}

impl Merge {
    fn id(self) -> &'static str {
        match self {
            Merge::Start => "merge.start",
            Merge::Queue => "merge.queue",
            Merge::Auto => "merge.auto",
        }
    }
}

/// Whether the head is still the commit the reader saw when they asked. A merge cannot be undone, so it names that commit and
/// refuses when the change has moved.
fn pin(seen: &str, now: &str) -> Result<(), String> {
    if seen.trim().is_empty() {
        return Err("A merge must name the commit the reader saw (`head_sha`), and none was given.".into());
    }
    if seen.eq_ignore_ascii_case(now) {
        Ok(())
    } else {
        Err("A new commit landed since you looked: the change is not the one you saw. Nothing was merged. Look at it again first.".into())
    }
}

/// The change as the forge says it is now, and only when merging it could do what the command says. The checks here are the
/// forge's own words; none is guessed.
fn ready_to_merge(change: &ChangeRef, which: Merge, seen: &str) -> Result<MineDetail, CommandError> {
    let detail = read::detail_on(change).map_err(CommandError::Forge)?;
    let (mine, base) = (&detail.mine, &detail.mine.base_branch);
    let said = |text: String| Err(CommandError::Invalid(text));
    pin(seen, &mine.head_sha).map_err(CommandError::Invalid)?;
    if mine.change.draft {
        return said(format!("{} is a draft. Mark it ready for review first.", change.reference()));
    }
    if mine.merge == MergeState::Conflicts {
        return said(format!("{} conflicts with {base}. Fix the conflict first; nothing was merged.", change.reference()));
    }
    match (which, &detail.queue) {
        (Merge::Start, Some(_)) => said(format!("{base} uses a merge queue, so a change is queued, not merged. Use merge.queue.")),
        (Merge::Start, None) if detail.queue_error.is_some() => {
            said(format!("The forge did not say whether {base} has a merge queue: {}", detail.queue_error.as_deref().unwrap_or_default()))
        }
        (Merge::Queue, None) if detail.queue_error.is_none() => {
            said(format!("{base} has no merge queue. Use merge.start, or merge.auto to merge when the checks pass."))
        }
        (Merge::Queue, None) => {
            said(format!("The forge did not say whether {base} has a merge queue: {}", detail.queue_error.as_deref().unwrap_or_default()))
        }
        _ => Ok(detail),
    }
}

/// What a merge that ran out of time says: the request may have reached the forge first.
fn merge_unknown(error: ForgeError, which: Merge) -> ForgeError {
    if !outcome_unknown(&error) {
        return error;
    }
    let now = match which {
        Merge::Start => "The change may already have merged: check it on the forge before merging again.",
        Merge::Queue => "The change may already be in the merge queue: check it on the forge before queueing again.",
        Merge::Auto => "Merge when ready may already be on: check the change on the forge before turning it on again.",
    };
    ForgeError::new(error.kind, error.message.replace(UNKNOWN_OUTCOME, now))
}

/// `merge.start`, `merge.queue` and `merge.auto`: asks the forge to merge, queue or merge when ready. Always asks: nothing is sent
/// without `confirm: true` and without the `head_sha` the reader saw (turning Merge when ready off needs neither, because it merges
/// nothing).
pub fn merge(which: Merge, p: MergeAsk) -> Result<Done, CommandError> {
    let _stale = Stale;
    if which == Merge::Auto && !p.enabled {
        act::cancel_auto(&p.change).send().map_err(CommandError::Forge)?;
        return done(&p.change, format!("Merge when ready is off for {}.", p.change.reference()));
    }
    refuse_without_confirm(p.confirm, which.id())?;
    pin(&p.head_sha, &p.head_sha).map_err(CommandError::Invalid)?;
    let detail = ready_to_merge(&p.change, which, &p.head_sha)?;
    let method =
        p.method.or_else(|| detail.methods.first().copied()).ok_or_else(|| CommandError::Invalid("The repository allows no merge method.".into()))?;
    if p.change.forge == Forge::GitHub && !detail.methods.contains(&method) {
        return Err(CommandError::Invalid(format!("The repository does not allow \"{}\".", method.label())));
    }
    let call =
        MergeCall { method, message: p.message, delete_branch: p.delete_branch, head_sha: p.head_sha.clone(), when_ready: which == Merge::Auto };
    act::merge(&p.change, &call).map_err(|e| CommandError::Invalid(e.message))?.send().map_err(|e| CommandError::Forge(merge_unknown(e, which)))?;
    let (what, base) = (p.change.reference(), &detail.mine.base_branch);
    let message = match which {
        Merge::Start => format!("Asked {} to merge {what} into {base} ({}).", p.change.forge.name(), method.label()),
        Merge::Queue => format!("Asked {} to add {what} to the merge queue of {base} ({}).", p.change.forge.name(), method.label()),
        Merge::Auto => format!("{what} will merge into {base} when its checks pass ({}).", method.label()),
    };
    done(&p.change, message)
}

/// Runs a `mine.*`, `checks.*`, `pr.*` or `merge.*` command. `None` when `id` is not one of them.
pub(crate) fn command(id: &str, value: &serde_json::Value) -> Option<Result<serde_json::Value, CommandError>> {
    use crate::params;
    let encode = |result: Result<serde_json::Value, serde_json::Error>| result.map_err(CommandError::from);
    // An explicit ask is a re-check: it reads the forges, never a held answer.
    if id == "mine.list" {
        invalidate();
    }
    Some(match id {
        "mine.list" => match value.get("forge") {
            None => encode(serde_json::to_value(list())),
            Some(forge) => match serde_json::from_value::<Forge>(forge.clone()) {
                Ok(forge) => encode(serde_json::to_value([source(forge)])),
                Err(e) => Err(CommandError::Params(e)),
            },
        },
        "mine.detail" => params::<On>(value).and_then(|p| encode(serde_json::to_value(detail(&p.change)?))),
        "checks.list" => params::<On>(value).and_then(|p| encode(serde_json::to_value(checks_list(&p.change)?))),
        "checks.log" => params::<LogOf>(value).and_then(|p| encode(serde_json::to_value(checks_log(&p.change, &p.check, p.all)?))),
        "checks.rerun" => params::<RerunOf>(value).and_then(|p| encode(serde_json::to_value(checks_rerun(&p.change, &p.check, p.confirm)?))),
        "pr.ready" => params::<On>(value).and_then(|p| encode(serde_json::to_value(pr_ready(&p.change)?))),
        "pr.set_reviewers" => params::<ReviewersOf>(value).and_then(|p| encode(serde_json::to_value(pr_set_reviewers(&p.change, &p.reviewers)?))),
        "pr.set_labels" => params::<LabelsOf>(value).and_then(|p| encode(serde_json::to_value(pr_set_labels(&p.change, &p.labels)?))),
        "pr.collaborators" => params::<On>(value).and_then(|p| encode(serde_json::to_value(pr_collaborators(&p.change)?))),
        "pr.labels" => params::<On>(value).and_then(|p| encode(serde_json::to_value(pr_labels(&p.change)?))),
        "merge.start" => params::<MergeAsk>(value).and_then(|p| encode(serde_json::to_value(merge(Merge::Start, p)?))),
        "merge.queue" => params::<MergeAsk>(value).and_then(|p| encode(serde_json::to_value(merge(Merge::Queue, p)?))),
        "merge.auto" => params::<MergeAsk>(value).and_then(|p| encode(serde_json::to_value(merge(Merge::Auto, p)?))),
        _ => return None,
    })
}

/// Every command id [`command`] takes.
pub const COMMANDS: [&str; 13] = [
    "mine.list",
    "mine.detail",
    "checks.list",
    "checks.log",
    "checks.rerun",
    "pr.ready",
    "pr.set_reviewers",
    "pr.set_labels",
    "pr.collaborators",
    "pr.labels",
    "merge.start",
    "merge.queue",
    "merge.auto",
];

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::run;

    fn change() -> serde_json::Value {
        json!({ "forge": "github", "repo": "acme/api", "number": 490, "host": "nowhere.invalid" })
    }

    #[test]
    fn merging_and_re_running_refuse_without_confirm_before_any_call_to_the_forge() {
        // The host does not exist: if a command reached the forge the error would say so, not ask for confirm.
        for (id, extra) in [
            ("merge.start", json!({ "method": "squash", "message": "m", "delete_branch": true, "head_sha": "abc" })),
            ("merge.queue", json!({ "method": "squash", "head_sha": "abc" })),
            ("merge.auto", json!({ "method": "squash", "head_sha": "abc" })),
            ("checks.rerun", json!({ "check": "1/2" })),
        ] {
            let mut params = extra;
            params["change"] = change();
            let refused = run(id, &params).expect_err("refused");
            assert!(matches!(&refused, CommandError::Invalid(text) if text.contains("confirm: true")), "{id}: {refused}");
            params["confirm"] = json!(false);
            assert!(run(id, &params).is_err(), "{id}: an explicit false is a refusal too");
        }
    }

    #[test]
    fn every_command_is_in_the_registry_and_a_merge_without_the_commit_it_saw_is_a_bad_request() {
        for id in COMMANDS {
            assert!(crate::COMMANDS.contains(&id), "{id} is missing from the registry");
        }
        let bad = run("merge.start", &json!({ "change": { "forge": "github" } })).expect_err("bad params");
        assert!(matches!(bad, CommandError::Params(_)));
        let no_sha = run("merge.start", &json!({ "change": change(), "method": "squash", "confirm": true })).expect_err("refused");
        assert!(matches!(&no_sha, CommandError::Invalid(text) if text.contains("head_sha")), "a merge names its commit, no fallback: {no_sha}");
        let off: MergeAsk = serde_json::from_value(json!({ "change": change(), "enabled": false })).expect("turning it off names no commit");
        assert!(!off.enabled && off.head_sha.is_empty());
        let empty = run("merge.start", &json!({ "change": change(), "head_sha": " ", "confirm": true })).expect_err("refused");
        assert!(matches!(&empty, CommandError::Invalid(text) if text.contains("head_sha")), "an empty sha names nothing: {empty}");
    }

    #[test]
    fn a_merge_is_refused_when_a_commit_landed_since_the_reader_looked() {
        assert!(pin("abc", "ABC").is_ok());
        assert!(pin("abc", "def").expect_err("moved").contains("new commit landed"));
        assert!(pin("", "def").is_err());
    }

    #[test]
    fn a_draft_the_forge_still_lists_as_a_draft_was_not_made_ready() {
        let mut m = tests_support::mine(1);
        assert!(verify_ready(&m).is_ok());
        m.change.draft = true;
        assert!(matches!(verify_ready(&m), Err(CommandError::Forge(text)) if text.message.contains("still lists")));
    }

    #[test]
    fn a_merge_that_ran_out_of_time_says_it_may_already_have_merged() {
        let slow = ForgeError::new(pr_craft_forge::FailKind::Timeout, format!("gh did not answer in time. {UNKNOWN_OUTCOME}"));
        let said = merge_unknown(slow.clone(), Merge::Start);
        assert!(said.message.contains("may already have merged"));
        assert!(!said.message.contains("post"));
        assert_eq!(said.kind, slow.kind);
        assert_eq!(merge_unknown("refused".into(), Merge::Start), "refused".into());
    }

    fn listed_source() -> MineSource {
        let state = MineState::Listed { changes: Vec::new(), truncated: false, hosts: Vec::new(), signed_out: Vec::new() };
        MineSource { forge: Forge::GitHub, cli: "gh", state }
    }

    /// Reads through `held_read` on a slot and a counter of its own, so no other test's write can touch them.
    struct Rig {
        slot: Mutex<Option<Held>>,
        generation: AtomicU64,
        reads: std::cell::Cell<u32>,
    }

    impl Rig {
        fn new() -> Self {
            Rig { slot: Mutex::new(None), generation: AtomicU64::new(0), reads: std::cell::Cell::new(0) }
        }

        fn read(&self, answer: impl FnOnce() -> MineSource) -> MineSource {
            held_read(&self.slot, &self.generation, || {
                self.reads.set(self.reads.get() + 1);
                answer()
            })
        }
    }

    #[test]
    fn a_read_inside_the_window_is_served_and_a_write_or_a_re_check_drops_it() {
        let rig = Rig::new();
        rig.read(listed_source);
        rig.read(listed_source);
        assert_eq!(rig.reads.get(), 1, "the second read is the first one's answer");
        rig.generation.fetch_add(1, Ordering::SeqCst);
        rig.read(listed_source);
        assert_eq!(rig.reads.get(), 2, "after an invalidate the forge is asked again");
    }

    #[test]
    fn a_failed_or_paused_read_is_not_held() {
        let rig = Rig::new();
        let failed = || MineSource { forge: Forge::GitHub, cli: "gh", state: MineState::Failed { error: "offline".into() } };
        let paused =
            || MineSource { forge: Forge::GitHub, cli: "gh", state: MineState::Paused { login: Login::LoggedOut { fix: "gh auth login".into() } } };
        rig.read(failed);
        rig.read(paused);
        rig.read(listed_source);
        assert_eq!(rig.reads.get(), 3, "each of them asked the forge again");
        rig.read(listed_source);
        assert_eq!(rig.reads.get(), 3, "a listing is held");
    }

    #[test]
    fn a_read_that_started_before_a_write_is_never_served_after_it() {
        let rig = Rig::new();
        let answered = rig.read(|| {
            // The write lands while the read is on its way.
            rig.generation.fetch_add(1, Ordering::SeqCst);
            listed_source()
        });
        assert!(matches!(answered.state, MineState::Listed { .. }), "its own caller still gets it");
        rig.read(listed_source);
        assert_eq!(rig.reads.get(), 2, "but the next caller does not get an answer that may predate the write");
    }

    #[test]
    fn every_write_command_drops_the_held_reads_whether_it_works_or_not() {
        let before = GENERATION.load(Ordering::SeqCst);
        let _ = checks_rerun(&serde_json::from_value(change()).expect("a change"), "1/2", false);
        assert!(GENERATION.load(Ordering::SeqCst) > before, "a refused write still drops them");
    }

    fn row(host: &str, listed: Result<(usize, bool), &str>) -> HostRead {
        (host.to_string(), listed.map(|(n, cut)| ((0..n as u64).map(tests_support::mine).collect(), cut)).map_err(ForgeError::from))
    }

    #[test]
    fn a_failed_host_keeps_the_others_listed_and_an_empty_list_is_not_an_all_clear() {
        let partial = joined(vec![row("github.com", Ok((0, false))), row("ghe.acme.dev", Err("401"))], vec![]);
        let MineState::Listed { hosts, changes, .. } = &partial else { panic!("one host answered: {partial:?}") };
        assert!(changes.is_empty());
        assert_eq!(hosts.iter().filter(|h| h.error.is_some()).count(), 1);
        assert!(partial.incomplete(), "a host that failed left changes out");
        assert!(
            !joined(vec![row("github.com", Ok((0, false)))], vec!["ghe.old".into()]).incomplete(),
            "a host never set up does not block the all-clear"
        );
        assert!(matches!(joined(vec![row("github.com", Err("401"))], vec![]), MineState::Failed { ref error } if error.contains("github.com")));
        let MineState::Listed { truncated, changes, .. } = joined(vec![row("github.com", Ok((2, true)))], vec![]) else { panic!("listed") };
        assert!(truncated && changes.len() == 2, "a full page is reported as partial");
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use pr_craft_core::mine::{Checks, Decision, MergeState, Mine};
    use pr_craft_core::{Change, Forge, Utc};

    /// An invented own change numbered `n`.
    pub(crate) fn mine(n: u64) -> Mine {
        Mine {
            change: Change {
                forge: Forge::GitHub,
                repo: "acme/api".into(),
                number: n,
                title: format!("Change {n}"),
                author: "maria".into(),
                url: format!("https://github.com/acme/api/pull/{n}"),
                host: None,
                updated_at: Utc::now(),
                draft: false,
                stack: None,
            },
            additions: Some(1),
            deletions: Some(1),
            head_branch: "b".into(),
            head_sha: "abc".into(),
            base_branch: "main".into(),
            commits: Some(1),
            reviewers: Vec::new(),
            requested: Vec::new(),
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
}
