//! `checkout.status`, `checkout.worktree`, `checkout.here` and `open.editor`: a change on this Mac, in the clones the user named
//! in Settings. Nothing outside those folders and the checkouts folder is read or written, and nothing here stashes, resets,
//! cleans or forces: a clone that is not safe to switch is left as it is, with the reason.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use pr_craft_core::ChangeRef;
use pr_craft_core::checkout::{self, Action, LocalState, Operation};
use pr_craft_core::diff::{self, FileStatus};
use pr_craft_core::editor::{self, Editor};
use pr_craft_core::gitref;
use pr_craft_core::settings::Settings;
use pr_craft_forge::bounded;
use pr_craft_forge::checkout::{NO_ROOM_FOR_HOOKS, no_hooks};
use pr_craft_forge::review as forge;
use serde::Serialize;

use crate::{CommandError, settings};

/// One checkout or one switch at a time, so a double press cannot run two git commands in the same clone.
static BUSY: Mutex<()> = Mutex::new(());

fn lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    BUSY.lock().map_err(|_| "an earlier checkout crashed; restart PR Craft".to_string())
}

fn invalid(error: String) -> CommandError {
    CommandError::Invalid(error)
}

// ---- git ----

/// What a read of git may take: a stalled one is stopped, and the lock it held with it.
const READ: Duration = Duration::from_secs(30);
/// What a fetch or a worktree add may take.
const FETCH: Duration = Duration::from_secs(180);
/// How long an editor's launcher may run before it is left to run on its own.
const LAUNCH_WAIT: Duration = Duration::from_secs(5);

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_for(dir, args, READ)
}

/// Runs git in `dir`. Never waits for a prompt, never takes the optional locks a plain read would (so a read leaves the index
/// as it found it), runs no hooks (see [`no_hooks`]: a worktree add or a fetch must not run a hook a branch brings), reads names
/// the way the Mac's volume does (`core.precomposeunicode`), and gives up after `limit`.
fn git_for(dir: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let hooks_off = no_hooks().ok_or(NO_ROOM_FOR_HOOKS)?;
    let out = bounded::run(
        Command::new("git")
            .args(["--no-optional-locks", "-c", "core.precomposeunicode=true"])
            .arg("-C")
            .arg(dir)
            .args(args)
            .envs(hooks_off)
            .env("GIT_TERMINAL_PROMPT", "0"),
        limit,
    )
    .map_err(|e| format!("`git {}` did not run: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("`git {}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// `(name, url)` of every remote of the clone.
fn remotes(clone: &Path) -> Vec<(String, String)> {
    // git config exits 1 when nothing matches: no remotes.
    let listed = git(clone, &["config", "--null", "--get-regexp", r"^remote\..*\.url$"]).unwrap_or_default();
    listed
        .split('\0')
        .filter_map(|entry| {
            let (key, url) = entry.split_once('\n')?;
            Some((key.strip_prefix("remote.")?.strip_suffix(".url")?.to_string(), url.to_string()))
        })
        .collect()
}

/// The remote of `clone` that is the change's repository on the change's host; `origin` first when several are.
fn remote_of(clone: &Path, change: &ChangeRef) -> Result<String, String> {
    let all = remotes(clone);
    let mut hits: Vec<&str> =
        all.iter().filter(|(_, url)| checkout::remote_matches(url, change.host_name(), &change.repo)).map(|(name, _)| name.as_str()).collect();
    hits.sort_by_key(|name| *name != "origin");
    let Some(name) = hits.first() else {
        let names: Vec<&str> = all.iter().map(|(name, _)| name.as_str()).collect();
        let have = if names.is_empty() { "it has no remotes".to_string() } else { format!("its remotes are {}", names.join(", ")) };
        return Err(format!("{} has no remote that points at {} on {}; {have}.", clone.display(), change.repo, change.host_name()));
    };
    gitref::check_name(name)?;
    Ok((*name).to_string())
}

/// The top folder of the git repository at `path`, resolved.
fn clone_root(path: &str) -> Result<PathBuf, String> {
    let dir = Path::new(path).canonicalize().map_err(|e| format!("{path} cannot be read: {e}"))?;
    let top = git(&dir, &["rev-parse", "--show-toplevel"]).map_err(|_| format!("{path} is not a git repository"))?;
    if Path::new(top.trim()).canonicalize().ok().as_deref() != Some(dir.as_path()) {
        return Err(format!("{path} is inside a git repository but is not its top folder; name the top folder"));
    }
    Ok(dir)
}

/// What the clone is doing and holding now. Reads only.
fn read_state(clone: &Path) -> Result<LocalState, String> {
    let out = git(clone, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=no"])?;
    let mut state = checkout::parse_status(&out);
    state.operation = [
        ("MERGE_HEAD", Operation::Merge),
        ("rebase-merge", Operation::Rebase),
        ("rebase-apply", Operation::Rebase),
        ("CHERRY_PICK_HEAD", Operation::CherryPick),
        ("REVERT_HEAD", Operation::Revert),
        ("BISECT_LOG", Operation::Bisect),
    ]
    .into_iter()
    .find(|(name, _)| git(clone, &["rev-parse", "--git-path", *name]).is_ok_and(|p| clone.join(p.trim()).exists()))
    .map(|(_, op)| op);
    if state.detached {
        state.unreachable_head = !git(clone, &["rev-list", "-n", "1", "HEAD", "--not", "--branches", "--tags", "--remotes"])?.trim().is_empty();
    }
    Ok(state)
}

/// [`read_state`], and the untracked and ignored files of the clone that a switch to a change with these `paths` would overwrite.
/// Git is asked about those paths only: a clone can hold hundreds of thousands of such files, and only one at a path the change
/// brings can be lost.
fn read_state_for(clone: &Path, paths: &[String]) -> Result<LocalState, String> {
    let mut state = read_state(clone)?;
    (state.untracked, state.ignored) = strays_among(clone, paths)?;
    state.untracked.extend(files_where_folders_go(clone, paths)?);
    Ok(state)
}

/// The untracked files and the ignored files at `paths` (or under one, where the clone has a folder and the change a file). Git
/// decides what is at a path: the pathspecs are literal and fold case (`icase`), and with `core.precomposeunicode` on (see
/// [`git_for`]) a name in either Unicode form finds the file the volume holds as one. Everything it lists counts; nothing here
/// compares names again.
fn strays_among(clone: &Path, paths: &[String]) -> Result<(Vec<String>, Vec<String>), String> {
    let specs: BTreeSet<String> =
        paths.iter().map(|p| p.trim_end_matches('/')).filter(|p| !p.is_empty()).map(|p| format!(":(literal,icase){p}")).collect();
    let specs: Vec<String> = specs.into_iter().collect();
    let (mut untracked, mut ignored) = (Vec::new(), Vec::new());
    for chunk in specs.chunks(400) {
        for (flags, found) in [(&["--exclude-standard"][..], &mut untracked), (&["--ignored", "--exclude-standard"][..], &mut ignored)] {
            let mut args = vec!["ls-files", "--others"];
            args.extend(flags);
            args.extend(["-z", "--"]);
            args.extend(chunk.iter().map(String::as_str));
            found.extend(git(clone, &args)?.split('\0').filter(|p| !p.is_empty()).map(str::to_string));
        }
    }
    Ok((untracked, ignored))
}

/// Folders above `paths` that are a file in the clone which git does not track: the change needs a folder there. The volume says
/// whether a name is a file (so case and Unicode form fold as it does); git says whether it is tracked.
fn files_where_folders_go(clone: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    let mut folders = BTreeSet::new();
    for path in paths {
        let mut at = path.trim_end_matches('/');
        while let Some((parent, _)) = at.rsplit_once('/') {
            if !folders.insert(parent.to_string()) {
                break;
            }
            at = parent;
        }
    }
    let mut found = Vec::new();
    for folder in folders {
        let in_the_way = clone.join(&folder).symlink_metadata().is_ok_and(|meta| !meta.is_dir());
        if in_the_way && git(clone, &["ls-files", "-z", "--", &format!(":(literal,icase){folder}")])?.trim_matches('\0').is_empty() {
            found.push(folder);
        }
    }
    Ok(found)
}

/// The ref the change's head is fetched into, in the clone: ours, so no other fetch rewrites it (FETCH_HEAD is everyone's).
fn private_ref(change: &ChangeRef) -> String {
    format!("refs/pr-craft/{}-{}", change.forge.cli(), change.number)
}

/// Fetches the change's head from `remote` into [`private_ref`] and checks it is the commit `want` the forge names.
fn fetch_head(clone: &Path, remote: &str, change: &ChangeRef, want: &str) -> Result<String, String> {
    let private = private_ref(change);
    git_for(clone, &["fetch", "--no-tags", "--quiet", "--", remote, &format!("+{}:{private}", checkout::head_ref(change))], FETCH)?;
    let head = git(clone, &["rev-parse", "--verify", "--quiet", &format!("{private}^{{commit}}")])?.trim().to_string();
    if head == want {
        Ok(head)
    } else {
        Err(format!(
            "{} says the change is at {want}, but fetching it gave {head}. The change may have moved just now; try again.",
            change.forge.name()
        ))
    }
}

/// Every path the switch from the clone's HEAD to the change's head rewrites: not only what the change's own diff names (against its
/// base), since a clone on an older commit differs from the head in more files. Needs the head in [`private_ref`].
fn rewritten_paths(clone: &Path, change: &ChangeRef, head: &str) -> Result<Vec<String>, String> {
    let have = git(clone, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", private_ref(change))])?;
    if have.trim() != head {
        return Err("the change's head is not fetched yet".into());
    }
    let names = git(clone, &["diff", "--name-only", "--no-renames", "-z", "HEAD", head])?;
    Ok(names.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect())
}

/// What the forge's diff names and what the switch rewrites, once each, when the head has been fetched before; the forge's alone before.
fn incoming_paths(clone: &Path, change: &ChangeRef, found: &Incoming) -> Vec<String> {
    let mut all = found.paths.clone();
    all.extend(rewritten_paths(clone, change, &found.head).unwrap_or_default());
    all.sort();
    all.dedup();
    all
}

/// Whether `target` is one of the worktrees of `clone` (the clone itself counts as one).
fn is_worktree(clone: &Path, target: &Path) -> bool {
    let same = |a: &Path, b: &Path| a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
    git(clone, &["worktree", "list", "--porcelain"])
        .is_ok_and(|out| out.lines().filter_map(|l| l.strip_prefix("worktree ")).any(|listed| same(Path::new(listed), target)))
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// `path` with the links of its nearest existing ancestor resolved, so a folder that is not made yet can still be compared with a
/// clone.
fn resolve_lenient(path: &Path) -> PathBuf {
    let (mut at, mut rest) = (path, Vec::new());
    loop {
        if let Ok(real) = at.canonicalize() {
            return rest.iter().rev().fold(real, |resolved, part| resolved.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

// ---- settings ----

fn setup(change: &ChangeRef) -> String {
    format!(
        "No local clone of {repo} is registered. In Settings → Repositories, choose {repo} and set its local folder to a clone of it.",
        repo = change.repo
    )
}

const NO_ROOT: &str = "Choose a checkouts folder in Settings → Repositories first; worktrees go there, one folder per change.";

/// What `settings.set` refuses beyond the shape of the values: a clone folder that is not the top of a git repository with a
/// remote that is that repository on that host, and a checkouts folder inside a clone (its worktrees would show up as untracked
/// files there). Only entries that changed are looked at, so a clone that moved away does not block saving something else.
pub fn check_settings(before: &Settings, after: &Settings) -> Result<(), String> {
    for (key, path) in after.clones.iter().filter(|(key, path)| before.clones.get(*key) != Some(*path)) {
        let (_, host, repo) = checkout::parse_clone_key(key).ok_or_else(|| format!("`{key}` is not a repository key"))?;
        let dir = clone_root(path)?;
        if !remotes(&dir).iter().any(|(_, url)| checkout::remote_matches(url, &host, &repo)) {
            let names: Vec<String> = remotes(&dir).into_iter().map(|(name, _)| name).collect();
            let have = if names.is_empty() { "it has no remotes".to_string() } else { format!("its remotes are {}", names.join(", ")) };
            return Err(format!("{path} has no remote that points at {repo} on {host}; {have}."));
        }
    }
    let root = Path::new(&after.checkouts_root);
    if after.checkouts_root.is_empty() || (before.checkouts_root == after.checkouts_root && before.clones == after.clones) {
        return Ok(());
    }
    if exists(root) && !root.is_dir() {
        return Err(format!("{} exists and is not a folder", root.display()));
    }
    let real = resolve_lenient(root);
    match after.clones.values().find(|clone| Path::new(clone).canonicalize().is_ok_and(|c| real.starts_with(c))) {
        Some(clone) => Err(format!("The checkouts folder {} is inside the clone {clone}; pick a folder outside every clone.", root.display())),
        None => Ok(()),
    }
}

// ---- what the forge says ----

/// What the forge says about the change that a checkout needs.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub branch: String,
    pub head: String,
    /// Every path the change adds or edits.
    pub paths: Vec<String>,
}

fn read_incoming(change: &ChangeRef) -> Result<Incoming, pr_craft_forge::ForgeError> {
    let (meta, text) = std::thread::scope(|scope| {
        let meta = scope.spawn(|| forge::change_meta(change));
        let text = forge::diff(change);
        (meta.join().unwrap_or_else(|_| Err("reading the change stopped".into())), text)
    });
    let (meta, text) = (meta?, text?);
    let paths = diff::parse_unified(&text).into_iter().filter(|f| f.status != FileStatus::Deleted).map(|f| f.path).collect();
    Ok(Incoming { branch: meta.head_branch, head: meta.head_sha, paths })
}

// ---- checkout.status ----

#[derive(Debug, Clone, Serialize)]
pub struct CloneStatus {
    /// The folder as registered in Settings.
    pub path: String,
    /// The remote that is the change's repository.
    pub remote: Option<String>,
    pub state: Option<LocalState>,
    /// Why the clone cannot be used, when it cannot.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub change: ChangeRef,
    pub checkouts_root: Option<String>,
    pub clone: Option<CloneStatus>,
    /// The change's worktree, when one exists.
    pub worktree: Option<String>,
    /// The worktree is at another commit than the change's head.
    pub worktree_other_commit: bool,
    /// Where its worktree is or would be made.
    pub worktree_path: Option<String>,
    pub head_branch: Option<String>,
    /// Where `open.editor` reads files: the worktree, else the clone when it is at the change's head commit.
    pub editor_root: Option<String>,
    pub worktree_action: Action,
    pub here_action: Action,
    /// Why the forge could not be read; "Check out here" waits for it.
    pub forge_error: Option<String>,
    /// What to do first, when there is no clone to use.
    pub setup: Option<String>,
}

/// The clone at `path` as it is now, and the paths the change would rewrite in it: the forge's diff, and, once the head has been
/// fetched, everything that differs from the clone's HEAD. Its ignored files are read for those paths only.
fn clone_status(path: &str, change: &ChangeRef, found: Option<&Incoming>) -> (CloneStatus, Option<PathBuf>) {
    let mut status = CloneStatus { path: path.to_string(), remote: None, state: None, error: None };
    let read = clone_root(path).and_then(|dir| {
        let remote = remote_of(&dir, change)?;
        let paths = found.map(|found| incoming_paths(&dir, change, found)).unwrap_or_default();
        Ok((remote, read_state_for(&dir, &paths)?, dir))
    });
    match read {
        Ok((remote, state, dir)) => {
            (status.remote, status.state) = (Some(remote), Some(state));
            (status, Some(dir))
        }
        Err(error) => {
            status.error = Some(error);
            (status, None)
        }
    }
}

/// `checkout.status`: reads this Mac and, when a clone is registered, the forge. Writes nothing.
pub fn status(change: &ChangeRef) -> Result<Status, CommandError> {
    let saved = settings::load()?;
    let incoming = saved.clones.contains_key(&checkout::change_key(change)).then(|| read_incoming(change));
    Ok(status_with(&saved, change, incoming.as_ref()))
}

/// [`status`] with the forge's answer in hand; `None` when it was not asked for.
pub fn status_with(saved: &Settings, change: &ChangeRef, incoming: Option<&Result<Incoming, pr_craft_forge::ForgeError>>) -> Status {
    let registered = saved.clones.get(&checkout::change_key(change));
    let (clone, dir) = match registered {
        Some(path) => {
            let (status, dir) = clone_status(path, change, incoming.and_then(|found| found.as_ref().ok()));
            (Some(status), dir)
        }
        None => (None, None),
    };
    let root = (!saved.checkouts_root.is_empty()).then(|| PathBuf::from(&saved.checkouts_root));
    let target = root.as_ref().map(|root| root.join(checkout::worktree_dir(change)));
    let existing = dir.as_ref().zip(target.as_ref()).filter(|(clone, target)| target.is_dir() && is_worktree(clone, target)).map(|(_, t)| t.clone());
    let head = incoming.and_then(|found| found.as_ref().ok());
    let state = clone.as_ref().and_then(|c| c.state.as_ref());
    let editor_root = existing
        .clone()
        .or_else(|| dir.clone().filter(|_| state.is_some_and(|s| s.head.is_some() && s.head.as_deref() == head.map(|h| h.head.as_str()))));

    // A worktree that is there is offered only when `checkout.worktree` would hand it back: with tracked files changed it refuses.
    let reuse = existing.as_ref().map(|path| match tracked_changes(path) {
        Ok(changes) if changes.is_empty() => Action::yes(),
        Ok(changes) => Action::no(unclean_reason(path, &changes)),
        Err(error) => Action::no(error),
    });
    let worktree_action = match (&clone, &dir, &root, &target) {
        (None, ..) => Action::no(setup(change)),
        (Some(c), None, ..) => Action::no(c.error.clone().unwrap_or_default()),
        (_, _, None, _) | (_, _, _, None) => Action::no(NO_ROOT),
        (Some(c), Some(dir), Some(root), Some(target)) => match () {
            _ if reuse.is_some() => reuse.clone().unwrap_or_else(Action::yes),
            _ if exists(target) => Action::no(format!("{} already exists and is not a worktree of {}.", target.display(), c.path)),
            _ if resolve_lenient(root).starts_with(dir) => {
                Action::no(format!("The checkouts folder {} is inside the clone {}; pick one outside it.", root.display(), c.path))
            }
            _ => Action::yes(),
        },
    };
    let here_action = match (&clone, state, incoming) {
        (None, ..) => Action::no(setup(change)),
        (Some(c), None, _) => Action::no(c.error.clone().unwrap_or_default()),
        (_, _, None) => Action::no("The forge has not been read yet."),
        (_, _, Some(Err(error))) => Action::no(format!(
            "The change could not be read from {}, so its files cannot be checked against the clone: {error}",
            change.forge.name()
        )),
        (_, Some(state), Some(Ok(found))) => match (hooks_blocker(), gitref::check_branch(&found.branch)) {
            (Some(why), _) => Action::no(why),
            (None, Err(error)) => Action::no(format!("The forge named a branch git would refuse: {error}")),
            (None, Ok(())) => Action::from_refusal(checkout::refuse_here(state)),
        },
    };
    Status {
        change: change.clone(),
        checkouts_root: root.map(|r| r.display().to_string()),
        clone,
        worktree_other_commit: existing
            .as_ref()
            .zip(head)
            .is_some_and(|(path, h)| git(path, &["rev-parse", "HEAD"]).is_ok_and(|have| have.trim() != h.head)),
        worktree: existing.map(|p| p.display().to_string()),
        worktree_path: target.map(|p| p.display().to_string()),
        head_branch: head.map(|h| h.branch.clone()),
        editor_root: editor_root.map(|p| p.display().to_string()),
        worktree_action,
        here_action,
        forge_error: incoming.and_then(|found| found.as_ref().err().map(|e| e.message.clone())),
        setup: registered.is_none().then(|| setup(change)),
    }
}

// ---- checkout.worktree ----

#[derive(Debug, Clone, Serialize)]
pub struct Worktree {
    pub path: String,
    /// The worktree was already there and has been left as it is.
    pub reused: bool,
    /// It is at another commit than the change's head: the change moved on, or the worktree was.
    pub other_commit: bool,
    pub head: Option<String>,
}

/// `checkout.worktree`.
pub fn worktree(change: &ChangeRef) -> Result<Worktree, CommandError> {
    let head = forge::change_meta(change).map_err(CommandError::Forge)?.head_sha;
    worktree_in(&settings::load()?, change, &head).map_err(invalid)
}

/// Makes, or finds, the change's worktree under the checkouts folder. It fetches the change's head from the forge's remote into
/// a ref of its own (never FETCH_HEAD, which another fetch rewrites), checks that it is the commit `want` the forge names, and adds
/// a detached worktree there, so no branch of the clone is touched and the clone's own checkout stays put. A worktree that is
/// already there is left as it is when it is clean, and `other_commit` says whether it is at another commit than `want`. A
/// worktree that fails to build is removed again, so no half-made folder is ever reported as reused.
pub fn worktree_in(saved: &Settings, change: &ChangeRef, want: &str) -> Result<Worktree, String> {
    worktree_with(saved, change, want, |clone, target, head| git_for(clone, &["worktree", "add", "--detach", "--", target, head], FETCH).map(drop))
}

/// [`worktree_in`] with the step that adds the worktree given (`clone`, `target`, `head`): git in production, and in a test a step
/// that fails the way a stopped add does, which cannot be made to happen at a chosen moment with a real one.
fn worktree_with(
    saved: &Settings,
    change: &ChangeRef,
    want: &str,
    add: impl FnOnce(&Path, &str, &str) -> Result<(), String>,
) -> Result<Worktree, String> {
    let _busy = lock()?;
    let path = saved.clones.get(&checkout::change_key(change)).ok_or_else(|| setup(change))?;
    let clone = clone_root(path)?;
    let remote = remote_of(&clone, change)?;
    if saved.checkouts_root.is_empty() {
        return Err(NO_ROOT.into());
    }
    if want.trim().is_empty() {
        return Err(format!("{} did not name the change's head commit, so a worktree cannot be checked against it.", change.forge.name()));
    }
    let root = PathBuf::from(&saved.checkouts_root);
    let target = root.join(checkout::worktree_dir(change));
    if is_worktree(&clone, &target) {
        if !target.is_dir() {
            return Err(format!(
                "{} is a worktree of {path} whose folder is gone. Run `git worktree prune` in the clone, then try again.",
                target.display()
            ));
        }
        let changes = tracked_changes(&target)?;
        if !changes.is_empty() {
            return Err(unclean_reason(&target, &changes));
        }
        let head = git(&target, &["rev-parse", "HEAD"]).ok().map(|h| h.trim().to_string());
        let other_commit = head.as_deref().is_some_and(|have| have != want);
        return Ok(Worktree { path: target.display().to_string(), reused: true, other_commit, head });
    }
    if exists(&target) {
        return Err(format!("{} already exists and is not a worktree of {path}; PR Craft will not create one inside it.", target.display()));
    }
    if resolve_lenient(&root).starts_with(&clone) {
        return Err(format!("The checkouts folder {} is inside the clone {path}; pick one outside it.", root.display()));
    }
    fs::create_dir_all(&root).map_err(|e| format!("{} could not be created: {e}", root.display()))?;
    let head = fetch_head(&clone, &remote, change, want)?;
    let target_arg = target.to_string_lossy();
    if let Err(error) = add(&clone, &target_arg, &head) {
        // This call made the folder (it did not exist a moment ago and the lock is held), so undo it: a stopped add leaves a
        // registered, half-filled worktree that the next press would take for a finished one.
        let _ = git(&clone, &["worktree", "remove", "--force", "--", &target_arg]);
        let _ = git(&clone, &["worktree", "prune"]);
        return Err(error);
    }
    Ok(Worktree { path: target.display().to_string(), reused: false, other_commit: false, head: Some(head) })
}

/// Why a worktree with `changes` is not handed back as the change's checkout.
fn unclean_reason(target: &Path, changes: &[String]) -> String {
    format!(
        "The worktree {} is not clean ({}), so PR Craft will not hand it back as the change's checkout. Look at it with git, or remove it with `git worktree remove` and try again.",
        target.display(),
        changes.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
    )
}

/// The major and minor of this Mac's git. A good read is kept for the life of the process; a failed one is not, so a git that was not
/// installed yet (the Command Line Tools shim) is found once it is.
fn git_version() -> Option<(u32, u32)> {
    static VERSION: OnceLock<(u32, u32)> = OnceLock::new();
    cached(&VERSION, || {
        let out = bounded::run(Command::new("git").arg("--version"), READ).ok()?;
        checkout::parse_git_version(&String::from_utf8_lossy(&out.stdout))
    })
}

/// `slot`'s value, or what `read` finds, which is kept in `slot` only when it is a value.
fn cached<T: Copy>(slot: &OnceLock<T>, read: impl FnOnce() -> Option<T>) -> Option<T> {
    if let Some(value) = slot.get() {
        return Some(*value);
    }
    let value = read()?;
    Some(*slot.get_or_init(|| value))
}

/// Why "Check out here" cannot keep a hook the change brings from running, when it cannot: no room for the setting in the
/// environment, or a git that would ignore it.
fn hooks_blocker() -> Option<String> {
    blocker(no_hooks().is_some(), git_version())
}

fn blocker(room: bool, version: Option<(u32, u32)>) -> Option<String> {
    if room { checkout::hooks_refusal(version) } else { Some(NO_ROOM_FOR_HOOKS.to_string()) }
}

/// The files of the worktree at `dir` that are changed, staged or missing against its HEAD. Untracked files are not asked about.
fn tracked_changes(dir: &Path) -> Result<Vec<String>, String> {
    let out = git(dir, &["status", "--porcelain", "--untracked-files=no", "-z"])?;
    Ok(out.split('\0').filter(|entry| !entry.is_empty()).map(|entry| entry.get(3..).unwrap_or(entry).to_string()).collect())
}

// ---- checkout.here ----

#[derive(Debug, Clone, Serialize)]
pub struct Here {
    pub clone: String,
    /// The branch the clone was on, `None` when it was detached.
    pub from: Option<String>,
    /// The branch it is on now.
    pub branch: Option<String>,
    pub head: Option<String>,
}

/// `checkout.here`: switches the registered clone to the change's branch with the forge CLI. `expect` is the branch the
/// confirm step named; a different one is refused, because the reader agreed to that one only.
pub fn here(change: &ChangeRef, confirm: bool, expect: Option<&str>) -> Result<Here, CommandError> {
    let saved = settings::load()?;
    let incoming = read_incoming(change).map_err(CommandError::Forge)?;
    here_with(&saved, change, &incoming, confirm, expect, |dir| pr_craft_forge::checkout::checkout_in(change, dir).map_err(|e| e.message))
        .map_err(invalid)
}

/// [`here`] with the forge's answer in hand and the switch given: the forge CLI in production.
pub fn here_with(
    saved: &Settings,
    change: &ChangeRef,
    incoming: &Incoming,
    confirm: bool,
    expect: Option<&str>,
    switch: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Here, String> {
    let _busy = lock()?;
    let seen = status_with(saved, change, Some(&Ok(incoming.clone())));
    if let Some(why) = seen.here_action.why_not {
        return Err(why);
    }
    let clone = seen.clone.ok_or_else(|| setup(change))?;
    let from = clone.state.and_then(|s| s.branch);
    if expect.is_some_and(|branch| branch != incoming.branch) {
        return Err(format!("{} now has the branch {}, not the one you were shown. Open the choice again.", change.reference(), incoming.branch));
    }
    if !confirm {
        let from_name = from.clone().unwrap_or_else(|| "a detached commit".into());
        return Err(format!(
            "Checking out here switches {} from {from_name} to {}. Nothing was changed; confirm to go ahead.",
            clone.path, incoming.branch
        ));
    }
    let dir = clone_root(&clone.path)?;
    let remote = clone.remote.clone().ok_or_else(|| setup(change))?;
    // The reader said yes: fetch the head, and judge the switch by everything it rewrites from the clone's HEAD, not only by the
    // change's own diff against its base. A clone on an older commit differs from the head in more files than that.
    fetch_head(&dir, &remote, change, &incoming.head)?;
    let mut paths = incoming.paths.clone();
    paths.extend(
        rewritten_paths(&dir, change, &incoming.head)
            .map_err(|e| format!("The files this switch rewrites could not be compared with the clone, so nothing was changed: {e}"))?,
    );
    paths.sort();
    paths.dedup();
    if let Some(why) = checkout::refuse_here(&read_state_for(&dir, &paths)?) {
        return Err(why);
    }
    switch(&dir)?;
    let after = read_state(&dir)?;
    // The forge CLI fetches the head again on its own, so the PR may have moved since it was vetted. Say so, and do not claim success.
    if after.head.as_deref() != Some(incoming.head.as_str()) {
        let short = |sha: &str| sha.chars().take(12).collect::<String>();
        let now = after.head.as_deref().map_or_else(|| "no commit".to_string(), short);
        return Err(format!(
            "The clone is now at {now}, not at the PR head {} that PR Craft checked. The PR may have moved while it was checked out, or the local branch already had commits of its own. PR Craft did not check what is at {now}; look at the clone before trusting it.",
            short(&incoming.head)
        ));
    }
    Ok(Here { clone: clone.path, from, branch: after.branch, head: after.head })
}

// ---- open.editor ----

#[derive(Debug, Clone, Serialize)]
pub struct Opened {
    pub editor: Editor,
    /// The checkout the file was read from.
    pub root: String,
    pub file: Option<String>,
    /// The command that ran, as a vector.
    pub command: Vec<String>,
    /// Something to know before reading: the worktree is at a different commit than the PR head.
    pub note: Option<String>,
}

/// `open.editor`: opens `path` (relative to the checkout) at `line` in `editor`, or the checkout itself in Terminal.
pub fn open_editor(change: &ChangeRef, editor: Editor, path: Option<&str>, line: Option<u32>) -> Result<Opened, CommandError> {
    let saved = settings::load()?;
    let want = forge::change_meta(change).map(|meta| meta.head_sha).map_err(|e| e.message);
    let (root, note) = editor_root(&saved, change, &want).map_err(invalid)?;
    launch(editor, &root, path, line, std::env::var_os("PATH").as_deref()).map(|opened| Opened { note, ..opened }).map_err(invalid)
}

/// What is said when a worktree is at another commit than the change's head.
fn other_commit_note(have: &str, want: &str) -> String {
    let short = |sha: &str| sha.chars().take(8).collect::<String>();
    format!(
        "The worktree is at a different commit than the PR head (it is at {}, the PR is at {}), so lines may not match the diff. PR Craft does not move it; update it with git in that folder.",
        short(have),
        short(want)
    )
}

/// The worktree of the change (with a note when it is at another commit than the PR head), else its clone when the clone is at the change's head
/// commit; never a clone on another commit, whose lines would not match the diff.
fn editor_root(saved: &Settings, change: &ChangeRef, want: &Result<String, String>) -> Result<(PathBuf, Option<String>), String> {
    let path = saved.clones.get(&checkout::change_key(change)).ok_or_else(|| setup(change))?;
    let clone = clone_root(path)?;
    remote_of(&clone, change)?;
    let target = (!saved.checkouts_root.is_empty()).then(|| Path::new(&saved.checkouts_root).join(checkout::worktree_dir(change)));
    if let Some(target) = target.filter(|t| t.is_dir() && is_worktree(&clone, t)) {
        let have = git(&target, &["rev-parse", "HEAD"]).ok().map(|h| h.trim().to_string());
        let note = match (&have, want) {
            (Some(have), Ok(want)) if have != want => Some(other_commit_note(have, want)),
            _ => None,
        };
        return Ok((target, note));
    }
    let want = want.as_ref().map_err(|e| format!("There is no worktree of this change, and the forge would not say which commit it is at: {e}"))?;
    match read_state(&clone)?.head {
        Some(have) if have == *want => Ok((clone, None)),
        _ => Err("This change is not checked out on this Mac. Check it out in a worktree first.".into()),
    }
}

/// `relative` inside `root`, resolved: it must exist, be a file, and stay inside the checkout after symbolic links are followed.
fn resolve_in(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = editor::inside(relative)?;
    let root = root.canonicalize().map_err(|e| format!("{} cannot be read: {e}", root.display()))?;
    let file = root.join(relative_path).canonicalize().map_err(|_| format!("{relative} is not in the checkout at {}", root.display()))?;
    if !file.starts_with(&root) {
        return Err(format!("{relative} leads outside the checkout, so it is not opened"));
    }
    if !file.is_file() {
        return Err(format!("{relative} is not a file"));
    }
    Ok(file)
}

fn is_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The command `tool` names: on the absolute folders of `path`, else at one of `fallbacks`.
fn find_tool(tool: &str, fallbacks: &[&str], path: Option<&OsStr>) -> Option<PathBuf> {
    let listed = path.into_iter().flat_map(std::env::split_paths).filter(|dir| dir.is_absolute()).map(|dir| dir.join(tool));
    listed.chain(fallbacks.iter().map(PathBuf::from)).find(|candidate| is_executable(candidate))
}

fn launch(editor: Editor, root: &Path, path: Option<&str>, line: Option<u32>, search: Option<&OsStr>) -> Result<Opened, String> {
    let (file, line) = match editor {
        Editor::Terminal => (None, 1),
        _ => {
            let path = path.ok_or_else(|| format!("{} needs the file to open", editor.label()))?;
            let line = line.filter(|l| (1..=10_000_000).contains(l)).ok_or("the line must be a number from 1")?;
            (Some(resolve_in(root, path)?), line)
        }
    };
    let tool = find_tool(editor.tool(), editor.fallbacks(), search)
        .ok_or_else(|| format!("{} is not installed here: no `{}` command was found. {}", editor.label(), editor.tool(), editor.fix()))?;
    let root = root.canonicalize().map_err(|e| format!("{} cannot be read: {e}", root.display()))?;
    let args = editor::args(editor, &root, file.as_deref().unwrap_or(&root), line);
    let command: Vec<String> =
        std::iter::once(tool.as_os_str()).chain(args.iter().map(|a| a.as_os_str())).map(|a| a.to_string_lossy().into_owned()).collect();
    // The launcher gets no pipes, so an editor that keeps running cannot hold this call; one that exits with an error is reported.
    bounded::launch(Command::new(&tool).args(&args).current_dir(&root), LAUNCH_WAIT)
        .map_err(|e| format!("{} could not start with `{}`: {e}", editor.label(), command.join(" ")))?;
    Ok(Opened { editor, root: root.display().to_string(), file: file.map(|f| f.display().to_string()), command, note: None })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use pr_craft_core::Forge;

    use super::*;

    /// A folder under the system temp dir that is removed when the test ends. Every checkout test runs in one of these and never
    /// near a real repository.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Tmp {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("pr-craft-local-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir_all(&path).expect("a temp folder");
            Tmp(path)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Runs git as a test author, returning whether it succeeded and what it printed.
    fn try_sh(dir: &Path, args: &[&str]) -> (bool, String) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "core.editor=true"])
            .args(args)
            .env("GIT_EDITOR", "true")
            .output()
            .expect("git runs");
        (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn sh(dir: &Path, args: &[&str]) -> String {
        let (ok, out) = try_sh(dir, args);
        assert!(ok, "git {args:?} failed in {}", dir.display());
        out
    }

    fn commit_file(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).expect("a file");
        sh(dir, &["add", name]);
        sh(dir, &["commit", "-m", name]);
    }

    struct Fixture {
        _tmp: Tmp,
        work: PathBuf,
        saved: Settings,
        change: ChangeRef,
        incoming: Incoming,
    }

    /// A clone on `main` whose `origin` URL names acme/api on github.com (rewritten to a bare repository on disk, so nothing
    /// leaves the machine), and a change #7 whose head ref holds `feature`, one commit that adds feature.txt.
    fn fixture() -> Fixture {
        let tmp = Tmp::new();
        let (origin, work) = (tmp.0.join("origin.git"), tmp.0.join("work"));
        fs::create_dir_all(&work).expect("work");
        sh(&tmp.0, &["init", "--bare", "-b", "main", "origin.git"]);
        sh(&work, &["init", "-b", "main"]);
        commit_file(&work, "README.md", "one\n");
        sh(&work, &["remote", "add", "origin", "https://github.com/acme/api.git"]);
        sh(&work, &["config", &format!("url.{}.insteadOf", origin.display()), "https://github.com/acme/api.git"]);
        sh(&work, &["push", "origin", "main"]);
        sh(&work, &["switch", "-c", "feature"]);
        commit_file(&work, "feature.txt", "feature\n");
        sh(&work, &["push", "origin", "feature", "feature:refs/pull/7/head"]);
        let head = sh(&work, &["rev-parse", "HEAD"]);
        sh(&work, &["switch", "main"]);
        let saved = Settings {
            checkouts_root: tmp.0.join("checkouts").display().to_string(),
            clones: BTreeMap::from([("gh:acme/api".to_string(), work.display().to_string())]),
            ..Settings::default()
        };
        let change = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 7, host: None };
        let incoming = Incoming { branch: "feature".into(), head, paths: vec!["feature.txt".into()] };
        Fixture { _tmp: tmp, work, saved, change, incoming }
    }

    impl Fixture {
        /// `here_with` and a switch that is git itself and counts how often it ran.
        fn here(&self, incoming: &Incoming, confirm: bool, ran: &Cell<u32>) -> Result<Here, String> {
            let work = self.work.clone();
            here_with(&self.saved, &self.change, incoming, confirm, None, |dir| {
                ran.set(ran.get() + 1);
                assert_eq!(dir.canonicalize().ok(), work.canonicalize().ok(), "the switch runs in the registered clone");
                sh(dir, &["switch", "feature"]);
                Ok(())
            })
        }

        fn branch(&self) -> Option<String> {
            read_state(&self.work).expect("state").branch
        }
    }

    #[test]
    fn a_clean_clone_switches_only_after_the_confirm_names_it() {
        let f = fixture();
        let ran = Cell::new(0);
        let asked = f.here(&f.incoming, false, &ran).expect_err("not confirmed");
        assert!(
            asked.contains("main") && asked.contains("feature") && asked.contains("work"),
            "the question names the clone and both branches: {asked}"
        );
        assert_eq!((ran.get(), f.branch().as_deref()), (0, Some("main")), "nothing ran before the confirm");
        let done = f.here(&f.incoming, true, &ran).expect("clean clone switches");
        assert_eq!((done.from.as_deref(), done.branch.as_deref()), (Some("main"), Some("feature")));
        assert_eq!(ran.get(), 1);
    }

    #[test]
    fn a_branch_the_reader_was_not_shown_is_not_switched_to() {
        let f = fixture();
        let ran = Cell::new(0);
        let shown = here_with(&f.saved, &f.change, &f.incoming, true, Some("other"), |_| {
            ran.set(1);
            Ok(())
        });
        assert!(shown.is_err() && ran.get() == 0);
    }

    #[test]
    fn uncommitted_work_is_never_switched_away_from() {
        let f = fixture();
        fs::write(f.work.join("README.md"), "mine\n").expect("edit");
        let ran = Cell::new(0);
        let refused = f.here(&f.incoming, true, &ran).expect_err("dirty");
        assert!(refused.contains("uncommitted"), "{refused}");
        assert_eq!((ran.get(), f.branch().as_deref()), (0, Some("main")));
        assert_eq!(fs::read_to_string(f.work.join("README.md")).expect("read"), "mine\n");
    }

    #[test]
    fn an_untracked_file_the_change_would_overwrite_blocks_the_switch_but_other_untracked_files_do_not() {
        let f = fixture();
        fs::write(f.work.join("scratch.txt"), "keep\n").expect("untracked");
        let ran = Cell::new(0);
        f.here(&f.incoming, true, &ran).expect("an unrelated untracked file is no reason to refuse");
        sh(&f.work, &["switch", "main"]);
        fs::write(f.work.join("feature.txt"), "precious\n").expect("untracked in the way");
        let refused = f.here(&f.incoming, true, &ran).expect_err("collision");
        assert!(refused.contains("feature.txt") && refused.contains("overwritten"), "{refused}");
        assert_eq!(ran.get(), 1, "the second try never reached git");
        assert_eq!(fs::read_to_string(f.work.join("feature.txt")).expect("read"), "precious\n");
    }

    #[test]
    fn an_ignored_file_the_change_would_track_blocks_the_switch_and_keeps_its_content() {
        let f = fixture();
        fs::write(f.work.join(".gitignore"), ".env\n").expect("ignore");
        sh(&f.work, &["add", ".gitignore"]);
        sh(&f.work, &["commit", "-m", "ignore env"]);
        fs::write(f.work.join(".env"), "local content\n").expect("ignored file");
        assert_eq!(read_state_for(&f.work, &[".env".into()]).expect("state").ignored, [".env"], "the ignored file is found for the path asked");
        let takes_env = Incoming { paths: vec![".env".into()], ..f.incoming.clone() };
        let ran = Cell::new(0);
        let refused = f.here(&takes_env, true, &ran).expect_err("the change would replace the local .env");
        assert!(refused.contains("Untracked or ignored") && refused.contains(".env"), "{refused}");
        assert_eq!(ran.get(), 0);
        assert_eq!(fs::read_to_string(f.work.join(".env")).expect("read"), "local content\n");
        f.here(&f.incoming, true, &ran).expect("an ignored file the change does not touch is no reason to refuse");
    }

    #[test]
    fn an_ignored_file_is_found_by_a_case_variant_of_its_name_and_only_for_the_paths_asked() {
        let f = fixture();
        fs::write(f.work.join(".gitignore"), "Settings.local.json\nbig/\n").expect("ignore");
        sh(&f.work, &["add", ".gitignore"]);
        sh(&f.work, &["commit", "-m", "ignore"]);
        fs::write(f.work.join("Settings.local.json"), "{}\n").expect("ignored file");
        fs::create_dir_all(f.work.join("big")).expect("dir");
        fs::write(f.work.join("big/cache.bin"), "x").expect("ignored file");
        let state = read_state_for(&f.work, &["settings.local.json".into(), "src/new.rs".into()]).expect("state");
        assert_eq!(
            state.ignored,
            ["Settings.local.json"],
            "a case variant is the same file on the default macOS volume; unasked ignored files are not listed"
        );
        assert!(checkout::refuse_here(&state).is_some());
        assert_eq!(
            read_state_for(&f.work, &["big".into()]).expect("state").ignored,
            ["big/cache.bin"],
            "a file where the change needs a folder, and the reverse"
        );
        assert_eq!(
            read_state_for(&f.work, &["big/cache.bin/deeper.txt".into()]).expect("state").untracked,
            ["big/cache.bin"],
            "a file where the change needs a folder is in the way, whether git ignores it or not"
        );
        fs::write(f.work.join("Scratch.txt"), "x").expect("untracked file");
        assert_eq!(
            read_state_for(&f.work, &["scratch.txt".into()]).expect("state").untracked,
            ["Scratch.txt"],
            "an untracked file is found by a case variant too, and only when asked for"
        );
    }

    /// Runs git with `input` on its stdin, returning what it printed.
    fn sh_in(dir: &Path, args: &[&str], input: &str) -> String {
        use std::io::Write;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("git runs");
        child.stdin.take().expect("stdin").write_all(input.as_bytes()).expect("input written");
        let out = child.wait_with_output().expect("git ends");
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn a_tree_entry_in_the_other_unicode_form_still_finds_the_file_the_volume_holds_as_one() {
        let f = fixture();
        let (nfc, nfd) = ("caf\u{e9}.local.json", "cafe\u{301}.local.json");
        fs::write(f.work.join(".gitignore"), "*.local.json\n").expect("ignore");
        sh(&f.work, &["add", ".gitignore"]);
        sh(&f.work, &["commit", "-m", "ignore"]);
        fs::write(f.work.join(nfc), "mine\n").expect("ignored file");
        if !f.work.join(nfd).exists() {
            // A volume that keeps the two forms apart (not APFS) cannot lose the file this way.
            return;
        }
        // A commit that tracks the decomposed name, as a hand-built tree or a non-Mac author can: the forge's diff names it so.
        let blob = sh_in(&f.work, &["hash-object", "-w", "--stdin"], "attacker\n");
        let tree = sh_in(&f.work, &["mktree"], &format!("100644 blob {blob}\t{nfd}\n"));
        let commit = sh(&f.work, &["commit-tree", &tree, "-p", "HEAD", "-m", "decomposed"]);
        sh(&f.work, &["push", "-f", "origin", &format!("{commit}:refs/pull/7/head")]);
        let incoming = Incoming { branch: "feature".into(), head: commit, paths: vec![nfd.to_string()] };
        let ran = Cell::new(0);
        let refused = f.here(&incoming, true, &ran).expect_err("the switch would replace the ignored file");
        assert!(refused.contains("Untracked or ignored"), "{refused}");
        assert_eq!(ran.get(), 0);
        assert_eq!(fs::read_to_string(f.work.join(nfc)).expect("read"), "mine\n");
    }

    #[test]
    fn a_switch_that_ends_somewhere_other_than_the_vetted_head_is_not_reported_as_done() {
        let f = fixture();
        let moved = here_with(&f.saved, &f.change, &f.incoming, true, None, |dir| {
            sh(dir, &["switch", "feature"]);
            commit_file(dir, "unvetted.txt", "x\n");
            Ok(())
        })
        .expect_err("the clone ended on a commit PR Craft never looked at");
        assert!(moved.contains("not at the PR head") && moved.contains("did not check"), "{moved}");
    }

    #[test]
    fn a_clone_on_an_older_commit_is_judged_by_everything_the_switch_rewrites() {
        let f = fixture();
        fs::write(f.work.join(".gitignore"), "config/local.json\n").expect("ignore");
        sh(&f.work, &["add", ".gitignore"]);
        sh(&f.work, &["commit", "-m", "ignore"]);
        // The change's head tracks config/local.json, but its diff against its own (newer) base does not name it.
        sh(&f.work, &["switch", "-c", "pr2", "main~1"]);
        fs::create_dir_all(f.work.join("config")).expect("dir");
        commit_file(&f.work, "config/local.json", "theirs\n");
        sh(&f.work, &["push", "-f", "origin", "pr2:refs/pull/7/head"]);
        let head = sh(&f.work, &["rev-parse", "HEAD"]);
        sh(&f.work, &["switch", "main"]);
        fs::create_dir_all(f.work.join("config")).expect("dir");
        fs::write(f.work.join("config/local.json"), "mine\n").expect("ignored file");
        let incoming = Incoming { branch: "pr2".into(), head, paths: vec!["unrelated.txt".into()] };
        assert!(status_with(&f.saved, &f.change, Some(&Ok(incoming.clone()))).here_action.allowed, "the forge's diff alone sees nothing in the way");
        let ran = Cell::new(0);
        let refused = f.here(&incoming, true, &ran).expect_err("the switch would replace the ignored file");
        assert!(refused.contains("config/local.json") && refused.contains("Untracked or ignored"), "{refused}");
        assert_eq!(ran.get(), 0);
        assert_eq!(fs::read_to_string(f.work.join("config/local.json")).expect("read"), "mine\n");
        assert!(!status_with(&f.saved, &f.change, Some(&Ok(incoming))).here_action.allowed, "and once fetched, the status says so too");
    }

    /// A clone whose `core.hooksPath` is `.husky`, and a change whose head adds `.husky/post-checkout`, a hook that leaves a marker.
    fn hooked(f: &Fixture) -> (Incoming, PathBuf) {
        sh(&f.work, &["config", "core.hooksPath", ".husky"]);
        sh(&f.work, &["switch", "-c", "hooked", "main"]);
        fs::create_dir_all(f.work.join(".husky")).expect("dir");
        fs::write(f.work.join(".husky/post-checkout"), "#!/bin/sh\ntouch hook-ran-marker\n").expect("hook");
        fs::set_permissions(f.work.join(".husky/post-checkout"), fs::Permissions::from_mode(0o755)).expect("chmod");
        sh(&f.work, &["add", ".husky"]);
        sh(&f.work, &["commit", "-m", "hook"]);
        sh(&f.work, &["push", "-f", "origin", "hooked:refs/pull/7/head"]);
        let head = sh(&f.work, &["rev-parse", "HEAD"]);
        sh(&f.work, &["switch", "main"]);
        (Incoming { branch: "hooked".into(), head, paths: vec![".husky/post-checkout".into()] }, f.work.join("hook-ran-marker"))
    }

    #[test]
    fn a_hook_a_change_brings_does_not_run_for_the_switch_or_the_worktree() {
        let f = fixture();
        let (incoming, marker) = hooked(&f);
        sh(&f.work, &["switch", "hooked"]);
        assert!(marker.exists(), "control: a plain switch runs the hook the branch brings, so the test can tell");
        fs::remove_file(&marker).expect("marker");
        sh(&f.work, &["switch", "main"]);
        let _ = fs::remove_file(&marker);
        here_with(&f.saved, &f.change, &incoming, true, None, |dir| {
            let status =
                Command::new("git").arg("-C").arg(dir).args(["switch", "hooked"]).envs(no_hooks().expect("room")).status().expect("git runs");
            assert!(status.success());
            Ok(())
        })
        .expect("the switch itself is allowed");
        assert_eq!(f.branch().as_deref(), Some("hooked"));
        assert!(!marker.exists(), "the checkout ran no hook");
        sh(&f.work, &["switch", "main"]);
        let made = worktree_in(&f.saved, &f.change, &incoming.head).expect("worktree");
        assert!(Path::new(&made.path).join(".husky/post-checkout").is_file());
        assert!(!Path::new(&made.path).join("hook-ran-marker").exists(), "no hook ran for the worktree either");
    }

    #[test]
    fn a_worktree_add_that_fails_leaves_nothing_behind_and_a_worktree_with_changes_is_not_reused() {
        let f = fixture();
        let target = Path::new(&f.saved.checkouts_root).join(checkout::worktree_dir(&f.change));
        // A real add that is then cut short: the worktree is registered and its folder is made, but the files are gone, as when a
        // slow checkout is killed. Nothing here depends on timing.
        let stopped = Cell::new(false);
        let error = worktree_with(&f.saved, &f.change, &f.incoming.head, |clone, target, head| {
            git(clone, &["worktree", "add", "--detach", "--", target, head])?;
            for entry in fs::read_dir(target).expect("the folder was made").flatten().filter(|e| e.file_name() != ".git") {
                let path = entry.path();
                if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }.expect("a file removed");
            }
            assert!(Path::new(target).is_dir() && sh(clone, &["worktree", "list", "--porcelain"]).contains("acme-api-7"), "the broken state is real");
            stopped.set(true);
            Err("the add was stopped".to_string())
        })
        .expect_err("the add failed");
        assert!(stopped.get() && error.contains("stopped"), "{error}");
        assert!(!target.exists(), "the half-made folder is gone");
        assert!(!sh(&f.work, &["worktree", "list", "--porcelain"]).contains("acme-api-7"), "and its registration with it");
        let made = worktree_in(&f.saved, &f.change, &f.incoming.head).expect("a later try builds it whole");
        assert!(!made.reused && Path::new(&made.path).join("feature.txt").is_file());
        sh(Path::new(&made.path), &["rm", "feature.txt"]);
        let refused = worktree_in(&f.saved, &f.change, &f.incoming.head).expect_err("a worktree with staged deletions is not clean");
        assert!(refused.contains("not clean") && refused.contains("feature.txt"), "{refused}");
        let offered = status_with(&f.saved, &f.change, Some(&Ok(f.incoming.clone()))).worktree_action;
        assert!(
            !offered.allowed && offered.why_not.is_some_and(|why| why.contains("not clean")),
            "the status does not offer what the command refuses"
        );
    }

    #[test]
    fn a_rebase_in_progress_blocks_the_switch() {
        let f = fixture();
        for (branch, text) in [("a", "from a\n"), ("b", "from b\n")] {
            sh(&f.work, &["switch", "-c", branch, "main"]);
            commit_file(&f.work, "README.md", text);
        }
        let (rebased, _) = try_sh(&f.work, &["rebase", "a"]);
        assert!(!rebased, "the two edits of one line conflict");
        assert_eq!(read_state(&f.work).expect("state").operation, Some(Operation::Rebase));
        let ran = Cell::new(0);
        let refused = f.here(&f.incoming, true, &ran).expect_err("mid-rebase");
        assert!(refused.contains("rebase"), "{refused}");
        assert_eq!(ran.get(), 0);
    }

    #[test]
    fn a_detached_head_is_left_only_when_a_branch_still_reaches_it() {
        let f = fixture();
        sh(&f.work, &["switch", "--detach", "main"]);
        assert!(status_with(&f.saved, &f.change, Some(&Ok(f.incoming.clone()))).here_action.allowed, "main still reaches it");
        commit_file(&f.work, "lost.txt", "x\n");
        let status = status_with(&f.saved, &f.change, Some(&Ok(f.incoming.clone())));
        assert!(!status.here_action.allowed, "a commit nothing reaches would be left behind");
    }

    #[test]
    fn a_branch_name_that_starts_with_a_dash_is_refused_before_any_command() {
        let f = fixture();
        let evil = Incoming { branch: "--upload-pack=sh".into(), ..f.incoming.clone() };
        let ran = Cell::new(0);
        let refused = f.here(&evil, true, &ran).expect_err("refused");
        assert!(refused.contains("branch"), "{refused}");
        assert_eq!(ran.get(), 0);
    }

    #[test]
    fn a_worktree_is_made_at_the_change_head_then_reused_and_the_clone_stays_put() {
        let f = fixture();
        let made = worktree_in(&f.saved, &f.change, &f.incoming.head).expect("worktree");
        assert!(!made.reused);
        assert_eq!(made.head.as_deref(), Some(f.incoming.head.as_str()), "it is at the head the forge's ref names");
        assert!(Path::new(&made.path).join("feature.txt").is_file());
        assert!(
            Path::new(&made.path).starts_with(&f.saved.checkouts_root)
                || Path::new(&made.path)
                    .canonicalize()
                    .is_ok_and(|p| p.starts_with(Path::new(&f.saved.checkouts_root).canonicalize().unwrap_or_default()))
        );
        let again = worktree_in(&f.saved, &f.change, &f.incoming.head).expect("reuse");
        assert!(again.reused && !again.other_commit);
        assert!(again.reused);
        assert_eq!(again.path, made.path);
        assert_eq!(f.branch().as_deref(), Some("main"), "the clone's own checkout did not move");
        let status = status_with(&f.saved, &f.change, Some(&Ok(f.incoming.clone())));
        assert_eq!(status.worktree.as_deref(), Some(made.path.as_str()));
        assert_eq!(status.editor_root, status.worktree, "files open from the worktree");
    }

    #[test]
    fn a_worktree_is_made_only_at_the_commit_the_forge_names_and_a_stale_one_is_reported_behind() {
        let f = fixture();
        let wrong = "0".repeat(40);
        let refused = worktree_in(&f.saved, &f.change, &wrong).expect_err("the fetched commit is not the one the forge named");
        assert!(refused.contains("fetching it gave"), "{refused}");
        assert!(
            !Path::new(&f.saved.checkouts_root).join(checkout::worktree_dir(&f.change)).exists(),
            "no worktree was made at a commit nobody named"
        );
        let made = worktree_in(&f.saved, &f.change, &f.incoming.head).expect("worktree");
        assert!(!made.other_commit);
        sh(&f.work, &["switch", "feature"]);
        commit_file(&f.work, "later.txt", "later\n");
        sh(&f.work, &["push", "-f", "origin", "feature:refs/pull/7/head"]);
        let moved = Incoming { head: sh(&f.work, &["rev-parse", "HEAD"]), ..f.incoming.clone() };
        sh(&f.work, &["switch", "main"]);
        let again = worktree_in(&f.saved, &f.change, &moved.head).expect("reused");
        assert!(again.reused && again.other_commit, "the change moved on after the worktree was made");
        assert!(status_with(&f.saved, &f.change, Some(&Ok(moved.clone()))).worktree_other_commit);
        let (root, note) = editor_root(&f.saved, &f.change, &Ok(moved.head.clone())).expect("a worktree to open from");
        assert!(root.ends_with(checkout::worktree_dir(&f.change)) && note.is_some_and(|n| n.contains("different commit than the PR head")));
    }

    #[test]
    fn a_worktree_is_never_made_inside_a_folder_that_is_not_a_worktree_or_inside_the_clone() {
        let f = fixture();
        let squatter = Path::new(&f.saved.checkouts_root).join(checkout::worktree_dir(&f.change));
        fs::create_dir_all(&squatter).expect("a plain folder");
        fs::write(squatter.join("mine.txt"), "x").expect("file");
        let refused = worktree_in(&f.saved, &f.change, &f.incoming.head).expect_err("exists");
        assert!(refused.contains("already exists"), "{refused}");
        assert!(squatter.join("mine.txt").is_file(), "what was there is untouched");
        let inside = Settings { checkouts_root: f.work.join("checkouts").display().to_string(), ..f.saved.clone() };
        let refused = worktree_in(&inside, &f.change, &f.incoming.head).expect_err("inside the clone");
        assert!(refused.contains("inside the clone"), "{refused}");
        assert!(!status_with(&inside, &f.change, None).worktree_action.allowed);
    }

    #[test]
    fn without_a_registered_clone_or_a_folder_the_status_says_what_to_set() {
        let f = fixture();
        let bare = Settings::default();
        let status = status_with(&bare, &f.change, None);
        assert!(!status.worktree_action.allowed && !status.here_action.allowed);
        assert!(status.setup.as_deref().is_some_and(|s| s.contains("Settings") && s.contains("acme/api")));
        assert!(worktree_in(&bare, &f.change, &f.incoming.head).is_err());
        let no_root = Settings { checkouts_root: String::new(), ..f.saved.clone() };
        let status = status_with(&no_root, &f.change, None);
        assert!(status.worktree_action.why_not.as_deref().is_some_and(|w| w.contains("checkouts folder")));
    }

    #[test]
    fn a_clone_is_registered_only_when_its_remote_is_that_repository_on_that_host() {
        let f = fixture();
        let none = Settings { clones: BTreeMap::new(), ..f.saved.clone() };
        check_settings(&none, &f.saved).expect("the clone's remote is acme/api on github.com");
        let other = |key: &str| Settings { clones: BTreeMap::from([(key.to_string(), f.work.display().to_string())]), ..f.saved.clone() };
        assert!(check_settings(&none, &other("gh:acme/other")).is_err(), "another repository");
        assert!(check_settings(&none, &other("glab:acme/api")).is_err(), "another host");
        let plain = Tmp::new();
        let elsewhere = Settings { clones: BTreeMap::from([("gh:acme/api".to_string(), plain.0.display().to_string())]), ..f.saved.clone() };
        assert!(check_settings(&none, &elsewhere).expect_err("not a repository").contains("not a git repository"));
        let nested = f.work.join("src");
        fs::create_dir_all(&nested).expect("subfolder");
        let sub = Settings { clones: BTreeMap::from([("gh:acme/api".to_string(), nested.display().to_string())]), ..f.saved.clone() };
        assert!(check_settings(&none, &sub).is_err(), "a subfolder is not the clone");
        let inside = Settings { checkouts_root: f.work.join("co").display().to_string(), ..f.saved.clone() };
        assert!(check_settings(&f.saved, &inside).expect_err("inside").contains("inside the clone"));
        assert!(check_settings(&inside, &inside).is_ok(), "settings already saved are not re-judged when nothing about them changed");
    }

    #[test]
    fn a_file_path_stays_inside_the_checkout() {
        let tmp = Tmp::new();
        let (root, outside) = (tmp.0.join("co"), tmp.0.join("outside"));
        fs::create_dir_all(root.join("src")).expect("dirs");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(root.join("src/a.rs"), "x").expect("file");
        fs::write(outside.join("secret.txt"), "x").expect("file");
        std::os::unix::fs::symlink(&outside, root.join("link")).expect("symlink");
        assert!(resolve_in(&root, "src/a.rs").expect("inside").ends_with("src/a.rs"));
        for bad in ["../outside/secret.txt", "src/../../outside/secret.txt", "/etc/hosts", "link/secret.txt", "src", "src/missing.rs", ""] {
            assert!(resolve_in(&root, bad).is_err(), "{bad:?} must not open");
        }
    }

    #[test]
    fn a_tool_is_found_on_an_absolute_folder_of_the_path_and_only_when_it_runs() {
        let tmp = Tmp::new();
        let (bin, other) = (tmp.0.join("bin"), tmp.0.join("other"));
        fs::create_dir_all(&bin).expect("dir");
        fs::create_dir_all(&other).expect("dir");
        let tool = bin.join("fake-editor");
        fs::write(&tool, "#!/bin/sh\n").expect("script");
        fs::write(other.join("fake-editor"), "not a program").expect("file");
        let search = std::env::join_paths([other.clone(), bin.clone(), PathBuf::from("relative")]).expect("path");
        assert_eq!(find_tool("fake-editor", &[], Some(&search)), None, "no execute bit, nothing found");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(find_tool("fake-editor", &[], Some(&search)), Some(tool.clone()));
        assert_eq!(find_tool("fake-editor", &[tool.to_str().expect("utf8")], None), Some(tool), "a fallback is used when the path has none");
        assert_eq!(find_tool("fake-editor", &[], None), None);
    }

    #[test]
    fn an_editor_is_not_started_for_a_path_outside_the_checkout_or_a_missing_line() {
        let tmp = Tmp::new();
        fs::write(tmp.0.join("a.rs"), "x").expect("file");
        for editor in [Editor::VsCode, Editor::Xcode, Editor::Zed] {
            assert!(launch(editor, &tmp.0, Some("../x"), Some(1), None).expect_err("outside").contains("not a path inside"));
            assert!(launch(editor, &tmp.0, Some("a.rs"), None, None).is_err(), "a line is needed");
            assert!(launch(editor, &tmp.0, Some("a.rs"), Some(0), None).is_err(), "lines count from 1");
            assert!(launch(editor, &tmp.0, None, Some(1), None).is_err(), "a file is needed");
        }
    }

    #[test]
    fn a_failed_read_of_gits_version_is_tried_again_and_a_good_one_is_kept() {
        let slot = OnceLock::new();
        assert_eq!(cached(&slot, || None::<(u32, u32)>), None);
        assert_eq!(cached(&slot, || Some((2, 53))), Some((2, 53)), "the first failure did not stick");
        assert_eq!(cached(&slot, || panic!("a version already read is not read again")), Some((2, 53)));
    }

    #[test]
    fn check_out_here_is_refused_when_hooks_cannot_be_kept_off() {
        assert!(blocker(false, Some((2, 53))).is_some_and(|why| why.contains("GIT_CONFIG_COUNT")), "no room for the setting");
        assert!(blocker(true, Some((2, 30))).is_some(), "a git that ignores it");
        assert_eq!(blocker(true, Some((2, 53))), None);
    }

    #[test]
    fn a_name_git_rejects_is_never_accepted() {
        let names = [
            "main", "a/b", "-x", "a..b", "a b", "a~", "a^", "a:b", "a?", "a*", "a[", "a\\b", "a/", "/a", "a//b", ".a", "a/.b", "a.lock", "a.",
            "a@{b", "ü", "a\tb",
        ];
        let probe = Tmp::new();
        for name in names {
            if name.starts_with('-') {
                assert!(gitref::check_branch(name).is_err());
                continue;
            }
            let (git_ok, _) = try_sh(&probe.0, &["check-ref-format", &format!("refs/heads/{name}")]);
            if !git_ok {
                assert!(gitref::check_branch(name).is_err(), "git rejects {name:?}, so must PR Craft");
            }
        }
    }
}
