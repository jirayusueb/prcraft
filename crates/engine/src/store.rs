//! Reviews in progress, saved file filters and notification state, as JSON files on this Mac. Nothing here talks to a forge.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pr_craft_core::agent::{Agent, EngineSettings};
use pr_craft_core::diff_view::LayoutOptions;
use pr_craft_core::extra::Extra;
use pr_craft_core::keys::Binding;
use pr_craft_core::review::LocalReview;
use pr_craft_core::run::{Run, RunState};
use pr_craft_core::settings::Settings as AppSettings;
use pr_craft_core::turn::Snapshot;
use pr_craft_core::view::FileView;
use pr_craft_core::{ChangeRef, DateTime, Forge, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// `$PR_CRAFT_DATA_DIR`, else `~/Library/Application Support/PR Craft`.
pub fn data_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("PR_CRAFT_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set, so there is nowhere to keep drafts")?;
    Ok(PathBuf::from(home).join("Library/Application Support/PR Craft"))
}

/// `reviews/<forge>/<repo>/<n>.json`; a self-hosted server gets its own `<forge>@<host>` folder, so two hosts with
/// the same project path never share drafts, and drafts saved before hosts existed stay where they were.
fn review_path(dir: &Path, change: &ChangeRef) -> PathBuf {
    let forge = match change.forge {
        Forge::GitHub => "github",
        Forge::GitLab => "gitlab",
    };
    let folder = change.host.as_ref().map_or_else(|| forge.to_string(), |host| format!("{forge}@{host}"));
    dir.join("reviews").join(folder).join(change.repo.replace('/', "%2F")).join(format!("{}.json", change.number))
}

/// `glab:group/repo`; a self-hosted server adds itself (`glab@gitlab.acme.dev:group/repo`), so the same path on two
/// hosts keeps two filters, and keys saved before hosts existed still match the default host.
fn filter_key(forge: Forge, host: Option<&str>, repo: &str) -> String {
    match host {
        Some(host) => format!("{}@{host}:{repo}", forge.cli()),
        None => format!("{}:{repo}", forge.cli()),
    }
}

/// A missing file is an empty review. A file that does not parse is an error, so drafts in it are never overwritten.
pub fn load_review(dir: &Path, change: &ChangeRef) -> Result<LocalReview, String> {
    load(&review_path(dir, change))
}

pub fn save_review(dir: &Path, change: &ChangeRef, review: &LocalReview) -> Result<(), String> {
    stamp(dir)?;
    save(&review_path(dir, change), review)
}

/// A change with something of the reader's on this Mac: drafts, a pending verdict or a general comment, or a post that may
/// have gone through. `edited` is when its file was last written.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pending {
    pub change: ChangeRef,
    pub drafts: usize,
    pub verdict: pr_craft_core::review::Verdict,
    pub post_unknown: Option<String>,
    pub edited: Option<DateTime<Utc>>,
}

/// Every review under `reviews/` that holds something, newest edit first, and the files that could not be read (never skipped
/// quietly: a draft in an unreadable file is still there). Reads this Mac only. A missing folder is no reviews.
pub fn list_pending(dir: &Path) -> (Vec<Pending>, Vec<String>) {
    let (mut found, mut unreadable) = (Vec::new(), Vec::new());
    // Only a folder that is not there is "nothing": any other failure is a place that may hold drafts and could not be read.
    // `folders` keeps only directories: a stray file (`.DS_Store`) between the levels is not a place that holds reviews.
    let mut entries = |path: &Path, folders: bool| -> Vec<fs::DirEntry> {
        let all: Vec<fs::DirEntry> = match fs::read_dir(path) {
            Ok(it) => it.filter_map(|entry| entry.map_err(|e| unreadable.push(format!("{}: {e}", path.display()))).ok()).collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                unreadable.push(format!("{}: {e}", path.display()));
                Vec::new()
            }
        };
        if !folders {
            return all;
        }
        all.into_iter()
            // `metadata` follows a link, so a repository folder that is a symlink to one elsewhere is read.
            .filter(|entry| match fs::metadata(entry.path()) {
                Ok(meta) => meta.is_dir(),
                Err(e) => {
                    unreadable.push(format!("{}: {e}", entry.path().display()));
                    false
                }
            })
            .collect()
    };
    let mut files = Vec::new();
    let mut unknown = Vec::new();
    for folder in entries(&dir.join("reviews"), true) {
        let name = folder.file_name().to_string_lossy().to_string();
        let (kind, host) = name.split_once('@').map_or((name.as_str(), None), |(kind, host)| (kind, Some(host.to_string())));
        let forge = match kind {
            "github" => Forge::GitHub,
            "gitlab" => Forge::GitLab,
            _ => {
                unknown.push(format!("{}: not a forge folder this build knows", folder.path().display()));
                continue;
            }
        };
        for repo in entries(&folder.path(), true) {
            let repo_name = repo.file_name().to_string_lossy().replace("%2F", "/");
            for file in entries(&repo.path(), false) {
                files.push((forge, host.clone(), repo_name.clone(), file.path()));
            }
        }
    }
    unreadable.extend(unknown);
    for (forge, host, repo, path) in files {
        let number = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u64>().ok());
        let (true, Some(number)) = (path.extension().is_some_and(|e| e == "json"), number) else { continue };
        let change = ChangeRef { forge, repo, number, host };
        match load::<LocalReview>(&path) {
            Ok(review) if review.is_empty() && review.post_unknown.is_none() => {}
            Ok(review) => {
                let edited = fs::metadata(&path).and_then(|m| m.modified()).ok().map(DateTime::<Utc>::from);
                found.push(Pending { change, drafts: review.drafts.len(), verdict: review.verdict, post_unknown: review.post_unknown, edited });
            }
            Err(error) => unreadable.push(format!("{}: {error}", path.display())),
        }
    }
    found.sort_by(|a, b| b.edited.cmp(&a.edited).then_with(|| a.change.reference().cmp(&b.change.reference())));
    (found, unreadable)
}

pub fn load_filter(dir: &Path, forge: Forge, host: Option<&str>, repo: &str) -> Result<Vec<String>, String> {
    let all: BTreeMap<String, Vec<String>> = load(&dir.join("filters.json"))?;
    Ok(all.get(&filter_key(forge, host, repo)).cloned().unwrap_or_default())
}

pub fn save_filter(dir: &Path, forge: Forge, host: Option<&str>, repo: &str, patterns: Vec<String>) -> Result<(), String> {
    let path = dir.join("filters.json");
    let mut all: BTreeMap<String, Vec<String>> = load(&path)?;
    let key = filter_key(forge, host, repo);
    if patterns.is_empty() {
        all.remove(&key);
    } else {
        all.insert(key, patterns);
    }
    stamp(dir)?;
    save(&path, &all)
}

/// The diff options chosen for one repository; the defaults when none were saved.
pub fn load_layout(dir: &Path, forge: Forge, host: Option<&str>, repo: &str) -> Result<LayoutOptions, String> {
    let all: BTreeMap<String, LayoutOptions> = load(&dir.join("layouts.json"))?;
    Ok(all.get(&filter_key(forge, host, repo)).cloned().unwrap_or_default())
}

pub fn save_layout(dir: &Path, forge: Forge, host: Option<&str>, repo: &str, options: LayoutOptions) -> Result<(), String> {
    let path = dir.join("layouts.json");
    let mut all: BTreeMap<String, LayoutOptions> = load(&path)?;
    let key = filter_key(forge, host, repo);
    if options == LayoutOptions::default() {
        all.remove(&key);
    } else {
        all.insert(key, options);
    }
    stamp(dir)?;
    save(&path, &all)
}

/// The view chosen for each file extension, by lowercase extension without its dot.
pub fn load_views(dir: &Path) -> Result<BTreeMap<String, FileView>, String> {
    let all: BTreeMap<String, serde_json::Value> = load(&dir.join("views.json"))?;
    Ok(all.into_iter().filter_map(|(ext, value)| Some((ext, serde_json::from_value(value).ok()?))).collect())
}

// shortcut: a view a newer build wrote is dropped on the next save by an older build; upgrade if views ever carry user data.
pub fn save_views(dir: &Path, views: &BTreeMap<String, FileView>) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("views.json"), views)
}

/// Which notifications the user has seen, and whether they are paused.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyState {
    pub seen: BTreeSet<String>,
    /// When each id was first seen (or first met by [`prune`], for ids saved before this field); ids older than 180 days are pruned.
    pub seen_at: BTreeMap<String, DateTime<Utc>>,
    pub pause: Option<Pause>,
    /// Events that got a macOS banner, posted or failed, so none gets a second one.
    pub bannered: BTreeSet<String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// `until: None` pauses until the user resumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    pub until: Option<DateTime<Utc>>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

pub fn load_notify(dir: &Path) -> Result<NotifyState, String> {
    load(&dir.join("notifications.json"))
}

pub fn save_notify(dir: &Path, state: &NotifyState) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("notifications.json"), state)
}

/// What Back to you last read of each change the user reviewed, so a poll reads again only the changes that moved. Keyed by
/// forge, host, repository and number.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotFile {
    pub entries: BTreeMap<String, SnapshotEntry>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// One change's last read: when the forge said it was last updated, the login it was read as, and the snapshot (`None`:
/// the forge holds no review of the user's on it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub updated_at: DateTime<Utc>,
    pub me: String,
    pub snapshot: Option<Snapshot>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

/// `gh|github.com|acme/api|12`; every key of one host starts with [`host_prefix`].
pub fn snapshot_key(change: &ChangeRef) -> String {
    format!("{}{}|{}", host_prefix(change.forge, change.host_name()), change.repo, change.number)
}

fn host_prefix(forge: Forge, host: &str) -> String {
    format!("{}|{host}|", forge.cli())
}

impl SnapshotFile {
    /// The stored read of `change` as `me`, when the forge still reports the update it was read at.
    pub fn unmoved(&self, change: &pr_craft_core::Change, me: &str) -> Option<&SnapshotEntry> {
        self.entries.get(&snapshot_key(&change.change_ref())).filter(|e| e.updated_at == change.updated_at && e.me == me)
    }

    /// Keeps `fresh` as the whole of `forge`'s entries, except those of the `skipped` hosts, which were not read and so
    /// are not known to be gone. Whatever is no longer listed is dropped.
    pub fn replace(&mut self, forge: Forge, skipped: &[String], fresh: Vec<(String, SnapshotEntry)>) {
        let kept =
            |key: &str| !key.starts_with(&format!("{}|", forge.cli())) || skipped.iter().any(|host| key.starts_with(&host_prefix(forge, host)));
        self.entries.retain(|key, _| kept(key));
        self.entries.extend(fresh);
    }
}

pub fn load_snapshots(dir: &Path) -> Result<SnapshotFile, String> {
    load(&dir.join("snapshots.json"))
}

pub fn save_snapshots(dir: &Path, file: &SnapshotFile) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("snapshots.json"), file)
}

fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{} is unreadable ({e}); it was left as it is", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("could not read {}: {e}", path.display())),
    }
}

/// A temp file name no other writer shares: the process id tells the GUI from the CLI, the counter tells one save from the next.
fn temp_path(path: &Path) -> PathBuf {
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{name}.{}.{}.tmp", std::process::id(), SAVES.fetch_add(1, Ordering::Relaxed)))
}

/// Writes a uniquely named sibling file and renames it over the old one, so a crash mid-write keeps the previous
/// drafts and two processes saving the same file never write the same temp file.
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let fail = |e: io::Error| format!("could not save {}: {e}", path.display());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(fail)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("could not encode {}: {e}", path.display()))?;
    let tmp = temp_path(path);
    let written = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        // Best effort: the save already failed, and the old file is untouched.
        let _ = fs::remove_file(&tmp);
    }
    written.map_err(fail)
}

/// The data version this build writes. A later build that changes a file's meaning, not just adds to it, bumps it.
const DATA_VERSION: u32 = 1;

/// The version a build wrote into `dir`, or `None` when no build has saved there yet or the file is not a number.
pub fn data_version(dir: &Path) -> Option<u32> {
    fs::read_to_string(dir.join("data-version")).ok()?.trim().parse().ok()
}

/// Writes `data-version` on the first save into `dir`; a version another build wrote stays as it is.
fn stamp(dir: &Path) -> Result<(), String> {
    if data_version(dir).is_some() {
        return Ok(());
    }
    let fail = |e: io::Error| format!("could not save {}: {e}", dir.join("data-version").display());
    fs::create_dir_all(dir).map_err(fail)?;
    fs::write(dir.join("data-version"), format!("{DATA_VERSION}\n")).map_err(fail)
}

pub fn load_tally(dir: &Path) -> Result<crate::tally::Tally, String> {
    load(&dir.join("tally.json"))
}

pub fn save_tally(dir: &Path, tally: &crate::tally::Tally) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("tally.json"), tally)
}

/// Appearance, accessibility and sync choices (Settings → Appearance and the pages after it).
pub fn load_settings(dir: &Path) -> Result<AppSettings, String> {
    load(&dir.join("settings.json"))
}

pub fn save_settings(dir: &Path, settings: &AppSettings) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("settings.json"), settings)
}

/// The shortcuts the user changed, by action id. An action that is not here keeps its default.
pub fn load_keys(dir: &Path) -> Result<BTreeMap<String, Binding>, String> {
    load(&dir.join("keys.json"))
}

pub fn save_keys(dir: &Path, keys: &BTreeMap<String, Binding>) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("keys.json"), keys)
}

/// The AI engine settings (Settings → AI engine).
pub fn load_ai(dir: &Path) -> Result<EngineSettings, String> {
    load(&dir.join("ai").join("settings.json"))
}

pub fn save_ai(dir: &Path, settings: &EngineSettings) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("ai").join("settings.json"), settings)
}

/// Estimated input tokens the engine started, by local day (`YYYY-MM-DD`).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub days: BTreeMap<String, u64>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

const USAGE_DAYS: usize = 31;

pub fn load_usage(dir: &Path) -> Result<Usage, String> {
    load(&dir.join("ai").join("usage.json"))
}

/// Adds `tokens` to `day` and keeps only the last 31 days.
pub fn add_usage(dir: &Path, day: &str, tokens: u64) -> Result<(), String> {
    let mut usage = load_usage(dir)?;
    let today = usage.days.entry(day.to_string()).or_insert(0);
    *today = today.saturating_add(tokens);
    while usage.days.len() > USAGE_DAYS {
        usage.days.pop_first();
    }
    stamp(dir)?;
    save(&dir.join("ai").join("usage.json"), &usage)
}

pub fn load_agents(dir: &Path) -> Result<Vec<Agent>, String> {
    load(&dir.join("ai").join("agents.json"))
}

pub fn save_agents(dir: &Path, agents: &[Agent]) -> Result<(), String> {
    stamp(dir)?;
    save(&dir.join("ai").join("agents.json"), &agents)
}

const REVIEW_DAYS: i64 = 90;
const KEEP_RUNS: usize = 500;
const SEEN_DAYS: i64 = 180;

/// Trims what only grows: empty review files untouched for 90 days, finished runs beyond the newest 500, and notification ids
/// older than 180 days. Never removes a draft, a general comment, a verdict, a post that may have gone through, or a run that is
/// still going. Never fails the caller: problems go to the log, with one line of counts.
pub fn prune(dir: &Path, now: DateTime<Utc>) {
    let reviews = prune_reviews(dir, now);
    let runs = prune_runs(dir);
    let seen = prune_seen(dir, now).unwrap_or_else(|error| {
        crate::log::note(&format!("prune: notifications: {error}"));
        0
    });
    if reviews + runs + seen > 0 {
        crate::log::note(&format!("prune: removed {reviews} reviews, {runs} runs, {seen} seen ids"));
    }
}

fn subdirs(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()
}

// shortcut: whether the change is closed is not checked against the forge, so age (file untouched 90 days) stands in for it; ask the forge when a closed date is stored.
fn prune_reviews(dir: &Path, now: DateTime<Utc>) -> usize {
    let cutoff = now - pr_craft_core::TimeDelta::days(REVIEW_DAYS);
    let mut removed = 0;
    for path in subdirs(&dir.join("reviews"))
        .iter()
        .flat_map(|forge| subdirs(forge))
        .flat_map(|repo| fs::read_dir(repo).into_iter().flatten().flatten())
        .map(|e| e.path())
    {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) else { continue };
        if DateTime::<Utc>::from(modified) >= cutoff {
            continue;
        }
        match load::<LocalReview>(&path) {
            Ok(review) if review.is_empty() && review.post_unknown.is_none() => match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) => crate::log::note(&format!("prune: {}: {error}", path.display())),
            },
            Ok(_) => {}
            Err(error) => crate::log::note(&format!("prune: {error}")),
        }
    }
    removed
}

fn prune_runs(dir: &Path) -> usize {
    let (runs, _) = list_runs(dir);
    let finished = |r: &&Run| matches!(r.state, RunState::Done | RunState::Failed | RunState::Stopped | RunState::Killed);
    let mut removed = 0;
    // `list_runs` is newest first, so what follows the first 500 finished runs is the oldest.
    for run in runs.iter().filter(finished).skip(KEEP_RUNS) {
        for extension in ["json", "lock", "signal"] {
            let Ok(path) = run_file(dir, &run.id, extension) else { continue };
            match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => crate::log::note(&format!("prune: {}: {error}", path.display())),
                _ => {}
            }
        }
        removed += 1;
    }
    removed
}

fn prune_seen(dir: &Path, now: DateTime<Utc>) -> Result<usize, String> {
    let Ok(_guard) = crate::turn::lock() else { return Err("the notification state is locked by a crash".into()) };
    let mut state = load_notify(dir)?;
    if state.seen.is_empty() && state.bannered.is_empty() && state.seen_at.is_empty() {
        return Ok(0);
    }
    for id in state.seen.iter().chain(&state.bannered) {
        state.seen_at.entry(id.clone()).or_insert(now);
    }
    let cutoff = now - pr_craft_core::TimeDelta::days(SEEN_DAYS);
    let old: Vec<String> = state.seen_at.iter().filter(|(_, at)| **at < cutoff).map(|(id, _)| id.clone()).collect();
    for id in &old {
        state.seen.remove(id);
        state.bannered.remove(id);
        state.seen_at.remove(id);
    }
    save_notify(dir, &state)?;
    Ok(old.len())
}

/// Run and agent ids become file names, so they are plain words only.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn run_file(dir: &Path, id: &str, extension: &str) -> Result<PathBuf, String> {
    if valid_id(id) { Ok(dir.join("runs").join(format!("{id}.{extension}"))) } else { Err(format!("`{id}` is not a run id")) }
}

/// `None` when there is no such run.
pub fn load_run(dir: &Path, id: &str) -> Result<Option<Run>, String> {
    load(&run_file(dir, id, "json")?)
}

/// Writes a new run's first file. Later changes go through [`with_run`], which merges.
pub fn create_run(dir: &Path, run: &Run) -> Result<(), String> {
    stamp(dir)?;
    with_lock(dir, &run.id, || save(&run_file(dir, &run.id, "json")?, run))
}

/// Holds the run's lock file, exclusively and across processes (the GUI and the CLI), while `work` runs.
fn with_lock<T>(dir: &Path, id: &str, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let path = run_file(dir, id, "lock")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("could not save {}: {e}", path.display()))?;
    }
    let file = fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
    file.lock().map_err(|e| format!("could not lock {}: {e}", path.display()))?;
    // The lock is released when `file` is dropped, after `work`.
    work()
}

/// The only way to change a saved run: under the run's lock it loads the file as it is now, lets `change` edit that copy
/// (touching only the fields it owns), and saves it if it changed. Nobody writes a snapshot they read earlier, so the
/// supervisor, `draft.keep`, `run.stop` and the orphan sweep never undo each other. `Ok(None)` when there is no such run.
pub fn with_run<T>(dir: &Path, id: &str, change: impl FnOnce(&mut Run) -> T) -> Result<Option<(Run, T)>, String> {
    with_lock(dir, id, || {
        let path = run_file(dir, id, "json")?;
        let Some(mut run) = load::<Option<Run>>(&path)? else { return Ok(None) };
        let before = run.clone();
        let out = change(&mut run);
        if run != before {
            save(&path, &run)?;
        }
        Ok(Some((run, out)))
    })
}

/// Every saved run, and the files that could not be read, so one damaged file does not hide the rest.
pub fn list_runs(dir: &Path) -> (Vec<Run>, Vec<String>) {
    let Ok(entries) = fs::read_dir(dir.join("runs")) else { return (Vec::new(), Vec::new()) };
    let (mut runs, mut unreadable) = (Vec::new(), Vec::new());
    for path in entries.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "json")) {
        match load::<Option<Run>>(&path) {
            Ok(run) => runs.extend(run),
            Err(error) => unreadable.push(error),
        }
    }
    runs.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    (runs, unreadable)
}

/// What the reviewer asked of a running run. The file is how another process (the CLI) reaches the one running it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Finish the current step, then end.
    Stop,
    /// End the process now.
    Kill,
}

/// Asks for `signal`. A kill is never downgraded to a stop.
pub fn send_signal(dir: &Path, id: &str, signal: Signal) -> Result<(), String> {
    let path = run_file(dir, id, "signal")?;
    if signal == Signal::Stop && read_signal(dir, id) == Some(Signal::Kill) {
        return Ok(());
    }
    let word = if signal == Signal::Kill { "kill" } else { "stop" };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("could not save {}: {e}", path.display()))?;
    }
    fs::write(&path, word).map_err(|e| format!("could not save {}: {e}", path.display()))
}

pub fn read_signal(dir: &Path, id: &str) -> Option<Signal> {
    let path = run_file(dir, id, "signal").ok()?;
    match fs::read_to_string(path).ok()?.trim() {
        "kill" => Some(Signal::Kill),
        "stop" => Some(Signal::Stop),
        _ => None,
    }
}

pub fn clear_signal(dir: &Path, id: &str) {
    if let Ok(path) = run_file(dir, id, "signal") {
        // Best effort: a leftover file only makes the next read of a finished run say stop.
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use pr_craft_core::review::{Draft, LineRef};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pr-craft-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn drafts_survive_a_round_trip_and_stay_per_change() {
        let dir = temp_dir("round-trip");
        let one = ChangeRef { forge: Forge::GitLab, repo: "group/sub/proj".into(), number: 7, host: None };
        let other = ChangeRef { number: 8, ..one.clone() };
        let mut review = LocalReview::default();
        review.drafts.push(Draft {
            id: 0,
            path: "a.go".into(),
            old_path: None,
            start: None,
            end: LineRef { old: Some(3), new: Some(4) },
            body: "why?".into(),
            on_rendered: true,
        });
        save_review(&dir, &one, &review).expect("save");
        assert_eq!(load_review(&dir, &one).expect("load"), review);
        assert!(load_review(&dir, &other).expect("load other").is_empty(), "another change starts empty");
        let elsewhere = ChangeRef { host: Some("gitlab.acme.dev".into()), ..one.clone() };
        assert!(load_review(&dir, &elsewhere).expect("load").is_empty(), "the same path and number on another host is another change");
        let saved_before_hosts: ChangeRef = serde_json::from_str(r#"{"forge":"gitlab","repo":"group/sub/proj","number":7}"#).expect("old key parses");
        assert_eq!(load_review(&dir, &saved_before_hosts).expect("load"), review, "a key without a host still finds its drafts");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_saves_leave_only_the_target_file_with_the_latest_content() {
        let dir = temp_dir("repeated-saves");
        let mut state = NotifyState::default();
        save_notify(&dir, &state).expect("save");
        state.seen.insert("x".into());
        save_notify(&dir, &state).expect("save again");
        assert_eq!(load_notify(&dir).expect("load").seen, state.seen);
        let temp = fs::read_dir(&dir).expect("dir").filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(temp, 0, "no temp file is left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_review_is_an_error_and_is_left_on_disk() {
        let dir = temp_dir("corrupt");
        let change = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 1, host: None };
        let path = review_path(&dir, &change);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, b"{not json").expect("write");
        assert!(load_review(&dir, &change).is_err());
        assert_eq!(fs::read(&path).expect("still there"), b"{not json");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pending_list_holds_only_changes_with_something_and_names_the_files_it_could_not_read() {
        let dir = temp_dir("pending");
        let draft = Draft {
            id: 0,
            path: "a.go".into(),
            old_path: None,
            start: None,
            end: LineRef { old: None, new: Some(1) },
            body: "x".into(),
            on_rendered: false,
        };
        let with_draft = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 482, host: None };
        let on_host = ChangeRef { forge: Forge::GitLab, repo: "group/sub/proj".into(), number: 7, host: Some("gitlab.acme.dev".into()) };
        let untouched = ChangeRef { number: 9, ..with_draft.clone() };
        let broken = ChangeRef { number: 10, ..with_draft.clone() };
        save_review(&dir, &with_draft, &LocalReview { drafts: vec![draft], ..LocalReview::default() }).expect("save");
        let verdict = LocalReview { verdict: pr_craft_core::review::Verdict::RequestChanges, ..LocalReview::default() };
        save_review(&dir, &on_host, &verdict).expect("save");
        save_review(&dir, &untouched, &LocalReview::default()).expect("save");
        let path = review_path(&dir, &broken);
        fs::write(&path, b"{not json").expect("write");
        let (found, unreadable) = list_pending(&dir);
        let mut changes: Vec<_> = found.iter().map(|p| (p.change.clone(), p.drafts)).collect();
        changes.sort_by_key(|(c, _)| c.number);
        assert_eq!(
            changes,
            vec![(on_host, 0), (with_draft, 1)],
            "a pending verdict counts, an untouched review does not, the host and the repo path come back"
        );
        assert!(found.iter().all(|p| p.edited.is_some()));
        assert_eq!(unreadable.len(), 1, "{unreadable:?}");
        assert!(unreadable[0].contains("10.json"), "{unreadable:?}");
        // Stray files Finder leaves between the levels are no reviews and no trouble.
        for stray in [dir.join("reviews/.DS_Store"), dir.join("reviews/github/.DS_Store")] {
            fs::write(stray, b"x").expect("write");
        }
        // A repository folder that is a symlink to one elsewhere is read like any other.
        {
            let elsewhere = dir.join("elsewhere");
            let target_change = ChangeRef { forge: Forge::GitHub, repo: "acme/linked".into(), number: 3, host: None };
            save_review(&elsewhere, &target_change, &LocalReview { verdict: pr_craft_core::review::Verdict::Approve, ..LocalReview::default() })
                .expect("save");
            let linked = dir.join("reviews/github/acme%2Flinkedrepo");
            std::os::unix::fs::symlink(review_path(&elsewhere, &target_change).parent().expect("parent"), &linked).expect("link");
            let (with_link, _) = list_pending(&dir);
            assert!(with_link.iter().any(|p| p.change.repo == "acme/linkedrepo" && p.change.number == 3), "{with_link:?}");
            fs::remove_file(&linked).expect("unlink");
        }
        let (after, unreadable_after) = list_pending(&dir);
        assert_eq!(after.len(), found.len(), "a stray file lists nothing");
        assert_eq!(unreadable_after.len(), unreadable.len(), "and is not named as unreadable: {unreadable_after:?}");
        assert_eq!(list_pending(&dir.join("nowhere")), (Vec::new(), Vec::new()), "no folder is no reviews");
        // A repository folder that cannot be listed may hold drafts: it is named, never read as "none".
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = review_path(&dir, &ChangeRef { number: 11, repo: "acme/locked".into(), forge: Forge::GitHub, host: None });
            let folder = locked.parent().expect("parent").to_path_buf();
            fs::create_dir_all(&folder).expect("mkdir");
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o000)).expect("lock");
            // A superuser can read it anyway; then there is nothing to name.
            let readable_anyway = fs::read_dir(&folder).is_ok();
            let (_, unreadable) = list_pending(&dir);
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).expect("unlock");
            assert!(readable_anyway || unreadable.iter().any(|u| u.contains("acme%2Flocked")), "{unreadable:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn layout_options_are_saved_per_repository_and_default_to_unified() {
        let dir = temp_dir("layouts");
        let split = LayoutOptions { split: true, wrap: true, ..LayoutOptions::default() };
        assert_eq!(load_layout(&dir, Forge::GitHub, None, "acme/api").expect("load"), LayoutOptions::default(), "nothing saved yet");
        save_layout(&dir, Forge::GitHub, None, "acme/api", split.clone()).expect("save");
        assert_eq!(load_layout(&dir, Forge::GitHub, None, "acme/api").expect("load"), split);
        assert_eq!(load_layout(&dir, Forge::GitHub, None, "acme/web").expect("load"), LayoutOptions::default(), "another repository is untouched");
        save_layout(&dir, Forge::GitHub, None, "acme/api", LayoutOptions::default()).expect("reset");
        assert_eq!(load_layout(&dir, Forge::GitHub, None, "acme/api").expect("load"), LayoutOptions::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn filters_are_saved_per_repository() {
        let dir = temp_dir("filters");
        save_filter(&dir, Forge::GitHub, None, "acme/api", vec!["internal/**".into()]).expect("save");
        save_filter(&dir, Forge::GitHub, None, "acme/web", vec!["!*.snap".into()]).expect("save");
        assert_eq!(load_filter(&dir, Forge::GitHub, None, "acme/api").expect("load"), vec!["internal/**".to_string()]);
        assert!(load_filter(&dir, Forge::GitLab, None, "acme/api").expect("load").is_empty(), "same path on the other forge is another repo");
        save_filter(&dir, Forge::GitLab, Some("gitlab.acme.dev"), "acme/api", vec!["docs/**".into()]).expect("save");
        assert!(load_filter(&dir, Forge::GitLab, None, "acme/api").expect("load").is_empty(), "same path on another host is another repo");
        assert_eq!(load_filter(&dir, Forge::GitLab, Some("gitlab.acme.dev"), "acme/api").expect("load"), vec!["docs/**".to_string()]);
        save_filter(&dir, Forge::GitHub, None, "acme/api", Vec::new()).expect("clear");
        assert!(load_filter(&dir, Forge::GitHub, None, "acme/api").expect("load").is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_view_choice_is_kept_per_extension() {
        let dir = temp_dir("views");
        assert!(load_views(&dir).expect("load").is_empty(), "nothing chosen yet");
        let mut views = BTreeMap::new();
        views.insert("md".to_string(), FileView::Source);
        views.insert("png".to_string(), FileView::ImageSwipe);
        save_views(&dir, &views).expect("save");
        assert_eq!(load_views(&dir).expect("load"), views);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn notify_state_and_pause_keep_unknown_keys() {
        let dir = temp_dir("notify-extra");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("notifications.json"), r#"{"pause": {"until": null, "why": "x"}, "from_the_future": 1}"#).expect("write");
        let state = load_notify(&dir).expect("loads");
        save_notify(&dir, &state).expect("saves");
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("notifications.json")).expect("read")).expect("json");
        assert_eq!(saved["from_the_future"], 1);
        assert_eq!(saved["pause"]["why"], "x");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_view_this_build_does_not_know_is_dropped_and_the_known_ones_stay() {
        let dir = temp_dir("views-lenient");
        fs::create_dir_all(&dir).expect("dir");
        let known = serde_json::to_value(FileView::Source).expect("encodes");
        fs::write(dir.join("views.json"), serde_json::json!({"md": known, "xyz": "hologram"}).to_string()).expect("write");
        let views = load_views(&dir).expect("loads");
        assert_eq!(views.keys().collect::<Vec<_>>(), ["md"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_drafts_and_an_unknown_post_survive_a_fresh_read() {
        let dir = temp_dir("restart");
        let change = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 3, host: None };
        let draft = |id| Draft {
            id,
            path: "a.rs".into(),
            old_path: None,
            start: None,
            end: LineRef { old: None, new: Some(1) },
            body: format!("d{id}"),
            on_rendered: false,
        };
        let review = LocalReview { drafts: vec![draft(0), draft(1)], post_unknown: Some("timed out".into()), ..LocalReview::default() };
        save_review(&dir, &change, &review).expect("save");
        let again = load_review(&dir, &change).expect("load");
        assert_eq!(again.drafts, review.drafts);
        assert_eq!(again.post_unknown.as_deref(), Some("timed out"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_data_version_is_written_on_the_first_save_and_an_unknown_forge_folder_is_reported() {
        let dir = temp_dir("data-version");
        assert_eq!(data_version(&dir), None);
        save_notify(&dir, &NotifyState::default()).expect("save");
        assert_eq!(data_version(&dir), Some(1));
        fs::create_dir_all(dir.join("reviews").join("bitbucket")).expect("dir");
        let (found, unreadable) = list_pending(&dir);
        assert!(found.is_empty());
        assert!(unreadable.iter().any(|line| line.contains("bitbucket")), "{unreadable:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    fn entry(day: u32, snapshot: Option<Snapshot>) -> SnapshotEntry {
        let updated_at = DateTime::parse_from_rfc3339(&format!("2026-11-{day:02}T09:00:00Z")).expect("a time").with_timezone(&Utc);
        SnapshotEntry { updated_at, me: "ana".into(), snapshot, extra: Extra::default() }
    }

    #[test]
    fn snapshots_keep_unknown_keys_and_a_no_review_entry_through_a_save() {
        let dir = temp_dir("snapshots-extra");
        fs::create_dir_all(&dir).expect("dir");
        let text = r#"{"entries":{"gh|github.com|acme/api|1":{"updated_at":"2026-11-02T09:00:00Z","me":"ana","snapshot":null,"newer":[1]}},"later":{"a":true}}"#;
        fs::write(dir.join("snapshots.json"), text).expect("write");
        let file = load_snapshots(&dir).expect("load");
        save_snapshots(&dir, &file).expect("save");
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("snapshots.json")).expect("read")).expect("json");
        assert_eq!(saved, serde_json::from_str::<serde_json::Value>(text).expect("json"), "both unknown keys came back");
        assert!(data_version(&dir).is_some(), "the data version is stamped on save");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_listed_host_keeps_only_what_it_still_lists_and_a_skipped_host_keeps_all() {
        let key = |forge, host: &str, number| snapshot_key(&ChangeRef { forge, repo: "acme/api".into(), number, host: Some(host.into()) });
        let mut file = SnapshotFile::default();
        for (forge, host, number) in
            [(Forge::GitLab, "a.example", 1), (Forge::GitLab, "a.example", 2), (Forge::GitLab, "b.example", 3), (Forge::GitHub, "github.com", 4)]
        {
            file.entries.insert(key(forge, host, number), entry(2, None));
        }
        file.replace(Forge::GitLab, &["b.example".to_string()], vec![(key(Forge::GitLab, "a.example", 1), entry(3, None))]);
        let kept: Vec<_> = file.entries.keys().cloned().collect();
        assert_eq!(kept, [key(Forge::GitHub, "github.com", 4), key(Forge::GitLab, "a.example", 1), key(Forge::GitLab, "b.example", 3)]);
        assert_eq!(file.entries[&key(Forge::GitLab, "a.example", 1)], entry(3, None), "a moved change takes the new read");
    }

    #[test]
    fn usage_keeps_the_last_31_days_and_unknown_keys() {
        let dir = temp_dir("usage");
        fs::create_dir_all(dir.join("ai")).expect("dir");
        fs::write(dir.join("ai/usage.json"), r#"{"days":{},"from_the_future":{"n":[1]}}"#).expect("seed");
        for day in 1..=40 {
            add_usage(&dir, &format!("2026-10-{day:02}"), 10).expect("add");
        }
        add_usage(&dir, "2026-10-40", 5).expect("add again");
        let usage = load_usage(&dir).expect("load");
        assert_eq!(usage.days.len(), 31);
        assert_eq!(usage.days.keys().next().map(String::as_str), Some("2026-10-10"));
        assert_eq!(usage.days["2026-10-40"], 15, "a day adds up");
        assert_eq!(serde_json::to_value(&usage).expect("save")["from_the_future"], serde_json::json!({"n": [1]}));
        let _ = fs::remove_dir_all(&dir);
    }

    fn aged(path: &Path, days: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).expect("open");
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400)).expect("age");
    }

    fn change(number: u64) -> ChangeRef {
        ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number, host: None }
    }

    #[test]
    fn prune_removes_only_old_empty_reviews() {
        let dir = temp_dir("prune-reviews");
        let draft = Draft {
            id: 1,
            path: "f.go".into(),
            old_path: None,
            start: None,
            end: LineRef { old: None, new: Some(2) },
            body: "keep me".into(),
            on_rendered: true,
        };
        let unknown = LocalReview { post_unknown: Some("timed out".into()), ..LocalReview::default() };
        let drafted = LocalReview { drafts: vec![draft], ..LocalReview::default() };
        for (n, review) in [(1, LocalReview::default()), (2, drafted), (3, unknown), (4, LocalReview::default())] {
            save_review(&dir, &change(n), &review).expect("save");
            if n != 4 {
                aged(&review_path(&dir, &change(n)), 100);
            }
        }
        prune(&dir, Utc::now());
        let there = |n| review_path(&dir, &change(n)).exists();
        assert_eq!([there(1), there(2), there(3), there(4)], [false, true, true, true]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_newest_500_finished_runs_and_every_running_one() {
        use pr_craft_core::agent::{Engine, SpawnLimits};
        use pr_craft_core::review::Anchor;
        use pr_craft_core::run::RunAgent;
        let dir = temp_dir("prune-runs");
        let base = Utc::now() - pr_craft_core::TimeDelta::days(1);
        let run = |id: String, state, minutes: i64| Run {
            id,
            change: change(1),
            change_title: String::new(),
            agent: RunAgent { id: None, name: "General review".into() },
            engine: Engine::ClaudeCode,
            state,
            started: base + pr_craft_core::TimeDelta::minutes(minutes),
            ended: None,
            last_output: None,
            pid: None,
            pid_start: None,
            owner_pid: 1,
            owner_start: None,
            anchor: Anchor { head_sha: "h".into(), base_sha: None, start_sha: None },
            files: Vec::new(),
            skipped: Vec::new(),
            redactions: 0,
            limits: SpawnLimits::default(),
            tree: Vec::new(),
            drafts: Vec::new(),
            dropped: 0,
            long_step: false,
            stopping: false,
            retries: 0,
            error: None,
            tail: Vec::new(),
            extra: Extra::default(),
        };
        for i in 0..503 {
            create_run(&dir, &run(format!("r{i}"), RunState::Done, i)).expect("save");
        }
        create_run(&dir, &run("live".into(), RunState::Running, -10_000)).expect("save");
        send_signal(&dir, "r0", Signal::Stop).expect("signal");
        prune(&dir, Utc::now());
        let left: Vec<String> = list_runs(&dir).0.into_iter().map(|r| r.id).collect();
        assert_eq!(left.len(), 501);
        assert!(left.contains(&"live".to_string()));
        for gone in ["r0", "r1", "r2"] {
            assert!(!left.contains(&gone.to_string()));
            assert!(!dir.join("runs").join(format!("{gone}.lock")).exists() && read_signal(&dir, gone).is_none());
        }
        assert!(left.contains(&"r3".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_ages_seen_ids_out_after_180_days_and_stamps_the_rest() {
        let dir = temp_dir("prune-seen");
        let now = Utc::now();
        let mut state = NotifyState::default();
        for id in ["old", "undated"] {
            state.seen.insert(id.to_string());
        }
        state.bannered.insert("old".into());
        state.seen_at.insert("old".into(), now - pr_craft_core::TimeDelta::days(181));
        save_notify(&dir, &state).expect("save");
        prune(&dir, now);
        let after = load_notify(&dir).expect("load");
        assert_eq!(after.seen.iter().collect::<Vec<_>>(), ["undated"]);
        assert!(after.bannered.is_empty());
        assert_eq!(after.seen_at.keys().collect::<Vec<_>>(), ["undated"]);
        assert_eq!(after.seen_at["undated"], now);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_on_a_missing_dir_does_nothing() {
        let dir = temp_dir("prune-missing");
        prune(&dir, Utc::now());
        assert!(!dir.exists());
    }
}
