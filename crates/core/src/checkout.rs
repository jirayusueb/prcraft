//! Opening a change on this Mac: which registered clone is its repository, what state that clone is in, and what may be done
//! to it. No I/O: the engine runs git and feeds the answers here.

use serde::Serialize;

use crate::{ChangeRef, Forge};

/// `gh:acme/api`, or `glab@gitlab.acme.dev:group/proj` on a server that is not the forge's default: where a repository's local
/// clone is filed in the settings. The forges ignore case in a path, so the key does too.
pub fn clone_key(forge: Forge, host: Option<&str>, repo: &str) -> String {
    let repo = repo.to_lowercase();
    match host.and_then(|h| forge.host_field(h)) {
        Some(host) => format!("{}@{host}:{repo}", forge.cli()),
        None => format!("{}:{repo}", forge.cli()),
    }
}

pub fn change_key(change: &ChangeRef) -> String {
    clone_key(change.forge, change.host.as_deref(), &change.repo)
}

/// The forge, host and repository a [`clone_key`] names.
pub fn parse_clone_key(key: &str) -> Option<(Forge, String, String)> {
    let (head, repo) = key.split_once(':')?;
    let (cli, host) = match head.split_once('@') {
        Some((cli, host)) => (cli, Some(host)),
        None => (head, None),
    };
    let forge = Forge::ALL.into_iter().find(|f| f.cli() == cli)?;
    let host = host.map_or_else(|| forge.default_host().to_string(), str::to_lowercase);
    valid_repo(repo).then(|| (forge, host, repo.to_string()))
}

/// Whether `repo` is a path of two parts or more that no command line could read as an option and no path could climb out of.
pub fn valid_repo(repo: &str) -> bool {
    let part = |p: &str| !p.is_empty() && p != "." && p != ".." && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    !repo.starts_with('-') && repo.split('/').count() >= 2 && repo.split('/').all(part)
}

/// The host and the repository path a git remote URL points at, from `https://host/a/b.git`, `ssh://git@host:22/a/b` and
/// `git@host:a/b.git`. The host is lowercase and has no port.
pub fn remote_target(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        None => {
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (authority, path)
        }
    };
    let host = authority.rsplit('@').next()?;
    let host = host.rsplit_once(':').filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit())).map_or(host, |(name, _)| name);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| (host.to_lowercase(), path.to_string()))
}

/// Whether the remote `url` is `repo` on `host`. A host alias from `~/.ssh/config` does not match; register a clone whose
/// remote names the server itself.
pub fn remote_matches(url: &str, host: &str, repo: &str) -> bool {
    remote_target(url).is_some_and(|(h, path)| h == host.to_lowercase() && path.eq_ignore_ascii_case(repo))
}

/// What a clone is in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Operation::Merge => "merge",
            Operation::Rebase => "rebase",
            Operation::CherryPick => "cherry-pick",
            Operation::Revert => "revert",
            Operation::Bisect => "bisect",
        }
    }
}

/// A clone as `git status --porcelain=v2 --branch -z --untracked-files=all` and a few other reads describe it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LocalState {
    /// The branch checked out; `None` when HEAD is detached.
    pub branch: Option<String>,
    pub detached: bool,
    /// The commit HEAD is at; `None` in a repository without a commit.
    pub head: Option<String>,
    /// Files with changes that are not committed, staged or not.
    pub changed: Vec<String>,
    /// Files with unresolved conflicts.
    pub conflicted: Vec<String>,
    /// Files git does not track and does not ignore that a switch would overwrite: the ones at, under or (where the change needs a
    /// folder) above a path the change brings. Git decides which, not this crate: it folds case and Unicode form as the volume
    /// does. Not the clone's whole list, which can be huge.
    pub untracked: Vec<String>,
    /// The same for files git ignores. A change that tracks one of these paths replaces it without a word.
    pub ignored: Vec<String>,
    pub operation: Option<Operation>,
    /// HEAD is detached on commits no branch, tag or remote branch reaches: switching away would leave them unreachable.
    pub unreachable_head: bool,
}

/// Reads the `-z` output of `git status --porcelain=v2 --branch --untracked-files=no`. The state of an operation in progress and
/// of a detached head is not in it; the caller sets `operation` and `unreachable_head`.
pub fn parse_status(out: &str) -> LocalState {
    let mut state = LocalState::default();
    let mut records = out.split('\0');
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            match header.split_once(' ') {
                Some(("branch.oid", oid)) => state.head = (oid != "(initial)").then(|| oid.to_string()),
                Some(("branch.head", "(detached)")) => state.detached = true,
                Some(("branch.head", name)) => state.branch = Some(name.to_string()),
                _ => {}
            }
            continue;
        }
        let (kind, fields) = match record.as_bytes().first() {
            Some(b'1') => ('1', 9),
            Some(b'2') => ('2', 10),
            Some(b'u') => ('u', 11),
            _ => continue,
        };
        let Some(path) = record.splitn(fields, ' ').nth(fields - 1) else { continue };
        match kind {
            'u' => state.conflicted.push(path.to_string()),
            _ => state.changed.push(path.to_string()),
        }
        if kind == '2' {
            // A rename or copy is followed by the path it came from.
            records.next();
        }
    }
    state
}

/// Why checking the change out in this clone would put work at risk, or `None` when nothing is. The untracked and ignored files
/// in `state` are those a switch would overwrite (see [`LocalState`]): git refuses over the first and not the second, and we
/// refuse over both, before asking.
pub fn refuse_here(state: &LocalState) -> Option<String> {
    if let Some(op) = state.operation {
        return Some(format!("The clone is in the middle of a {}. Finish or abort it first.", op.name()));
    }
    if !state.conflicted.is_empty() {
        return Some(format!("The clone has unresolved conflicts ({}). Resolve them first.", sample(&state.conflicted)));
    }
    if !state.changed.is_empty() {
        return Some(format!("The clone has uncommitted changes ({}). Commit or stash them yourself first.", sample(&state.changed)));
    }
    if state.detached && state.unreachable_head {
        return Some("The clone is on a detached commit no branch reaches; switching would leave it behind. Make a branch there first.".into());
    }
    let clash: Vec<String> = state.untracked.iter().chain(&state.ignored).cloned().collect();
    if !clash.is_empty() {
        return Some(format!(
            "Untracked or ignored files in the clone would be overwritten by this change ({}). Move or commit them first.",
            sample(&clash)
        ));
    }
    None
}

/// The major and minor of `git --version` (`git version 2.53.0 (Apple Git-155)`).
pub fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().strip_prefix("git version ")?.split(['.', ' ']);
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The first git that reads `GIT_CONFIG_COUNT`, which is how the forge crate's `no_hooks` turns hooks off.
pub const HOOKS_OFF_FROM: (u32, u32) = (2, 31);

/// Why the switch cannot be done without hooks, when the git on this Mac is too old to turn them off (it would ignore the setting
/// and run a hook a branch brings), or its version cannot be read.
pub fn hooks_refusal(version: Option<(u32, u32)>) -> Option<String> {
    match version {
        Some(have) if have >= HOOKS_OFF_FROM => None,
        Some((major, minor)) => Some(format!(
            "Check out here needs git {}.{} or newer, to keep a hook the change brings from running; this Mac has git {major}.{minor}. Update git (brew install git), or use a worktree.",
            HOOKS_OFF_FROM.0, HOOKS_OFF_FROM.1
        )),
        None => Some(
            "Check out here needs git 2.31 or newer, to keep a hook the change brings from running, and `git --version` could not be read.".into(),
        ),
    }
}

fn sample(paths: &[String]) -> String {
    let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
    let more = paths.len().saturating_sub(shown.len());
    if more > 0 { format!("{}, and {more} more", shown.join(", ")) } else { shown.join(", ") }
}

/// The ref the forge keeps for a change's head commit, fetchable even when the change comes from a fork.
pub fn head_ref(change: &ChangeRef) -> String {
    match change.forge {
        Forge::GitHub => format!("refs/pull/{}/head", change.number),
        Forge::GitLab => format!("refs/merge-requests/{}/head", change.number),
    }
}

/// The folder of a change's worktree under the checkouts root: `acme-api-482`, with the host first on a self-hosted server.
pub fn worktree_dir(change: &ChangeRef) -> String {
    let host = change.host.as_deref().and_then(|h| change.forge.host_field(h));
    let raw = format!("{}{}-{}", host.map(|h| format!("{h}-")).unwrap_or_default(), change.repo.replace('/', "-"), change.number);
    let safe: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    safe.trim_start_matches(['.', '-']).to_string()
}

/// One thing the popover offers: whether it may run now, and if not, the reason in words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    pub allowed: bool,
    pub why_not: Option<String>,
}

impl Action {
    pub fn yes() -> Self {
        Action { allowed: true, why_not: None }
    }

    pub fn no(why: impl Into<String>) -> Self {
        Action { allowed: false, why_not: Some(why.into()) }
    }

    pub fn from_refusal(refusal: Option<String>) -> Self {
        refusal.map_or_else(Action::yes, Action::no)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(forge: Forge, repo: &str, host: Option<&str>) -> ChangeRef {
        ChangeRef { forge, repo: repo.into(), number: 7, host: host.map(str::to_string) }
    }

    #[test]
    fn a_remote_matches_its_repository_on_its_own_host_in_every_url_shape() {
        for url in [
            "https://github.com/Acme/api.git",
            "https://maya@github.com/acme/api",
            "git@github.com:acme/api.git",
            "ssh://git@github.com:22/acme/api.git",
            "git://github.com/acme/api/",
        ] {
            assert!(remote_matches(url, "github.com", "acme/api"), "{url}");
            assert!(!remote_matches(url, "gitlab.com", "acme/api"), "{url} is not on another host");
            assert!(!remote_matches(url, "github.com", "acme/other"), "{url} is not another repository");
        }
        assert!(remote_matches("git@gitlab.acme.dev:grp/sub/proj.git", "gitlab.acme.dev", "grp/sub/proj"));
        assert!(!remote_matches("/tmp/somewhere/acme/api", "github.com", "acme/api"), "a local path names no host");
    }

    #[test]
    fn a_clone_key_round_trips_and_a_default_host_leaves_no_host_in_it() {
        let github = change(Forge::GitHub, "Acme/Api", Some("github.com"));
        assert_eq!(change_key(&github), "gh:acme/api");
        let own = change(Forge::GitLab, "grp/proj", Some("gitlab.acme.dev"));
        assert_eq!(change_key(&own), "glab@gitlab.acme.dev:grp/proj");
        assert_eq!(parse_clone_key("glab@gitlab.acme.dev:grp/proj"), Some((Forge::GitLab, "gitlab.acme.dev".into(), "grp/proj".into())));
        assert_eq!(parse_clone_key("gh:acme/api"), Some((Forge::GitHub, "github.com".into(), "acme/api".into())));
        for bad in ["gh:-x/api", "gh:acme", "gh:../x", "hg:acme/api", "acme/api", "gh:a/b c"] {
            assert_eq!(parse_clone_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn porcelain_v2_reads_the_branch_the_changes_and_the_conflicts() {
        let out = "# branch.oid 5d41402abc4b2a76b9719d911017c592\0# branch.head feature/x\0# branch.upstream origin/feature/x\0# branch.ab +1 -0\0\
                   1 .M N... 100644 100644 100644 aaa bbb src/a b.rs\0\
                   2 R. N... 100644 100644 100644 aaa bbb R100 new name.rs\0old name.rs\0\
                   u UU N... 100644 100644 100644 100644 a b c conflicted.rs\0";
        let state = parse_status(out);
        assert_eq!(state.branch.as_deref(), Some("feature/x"));
        assert!(!state.detached);
        assert_eq!(state.head.as_deref(), Some("5d41402abc4b2a76b9719d911017c592"));
        assert_eq!(state.changed, ["src/a b.rs", "new name.rs"], "a rename's old path is not another change");
        assert_eq!(state.conflicted, ["conflicted.rs"]);
        let detached = parse_status("# branch.oid abc\0# branch.head (detached)\0");
        assert!(detached.detached && detached.branch.is_none());
        assert!(parse_status("# branch.oid (initial)\0# branch.head main\0").head.is_none());
    }

    #[test]
    fn nothing_is_checked_out_over_work_that_would_be_lost() {
        let clean = LocalState { branch: Some("main".into()), head: Some("a".into()), ..LocalState::default() };
        assert_eq!(refuse_here(&clean), None);
        let with = |edit: fn(&mut LocalState)| {
            let mut s = clean.clone();
            edit(&mut s);
            refuse_here(&s).expect("refused")
        };
        assert!(with(|s| s.changed.push("x".into())).contains("uncommitted"));
        assert!(with(|s| s.conflicted.push("x".into())).contains("conflicts"));
        assert!(with(|s| s.operation = Some(Operation::Rebase)).contains("rebase"));
        assert!(with(|s| (s.detached, s.unreachable_head) = (true, true)).contains("detached"));
        let overwritten = with(|s| s.untracked.push("a.rs".into()));
        assert!(overwritten.contains("Untracked or ignored") && overwritten.contains("a.rs"), "{overwritten}");
        assert!(with(|s| s.ignored.push(".env".into())).contains(".env"), "an ignored file the switch would replace counts like an untracked one");
        let mut tag = clean.clone();
        (tag.detached, tag.branch) = (true, None);
        assert_eq!(refuse_here(&tag), None, "a detached head a branch or tag reaches loses nothing");
    }

    #[test]
    fn the_hooks_switch_is_refused_on_a_git_that_would_ignore_it() {
        assert_eq!(parse_git_version("git version 2.53.0 (Apple Git-155)\n"), Some((2, 53)));
        assert_eq!(parse_git_version("git version 2.30.1"), Some((2, 30)));
        assert_eq!(parse_git_version("not git"), None);
        assert_eq!(hooks_refusal(Some((2, 31))), None);
        assert_eq!(hooks_refusal(Some((3, 0))), None);
        assert!(hooks_refusal(Some((2, 30))).is_some_and(|why| why.contains("2.31") && why.contains("2.30")));
        assert!(hooks_refusal(None).is_some());
    }

    #[test]
    fn a_worktree_folder_is_one_safe_name_that_differs_per_change_and_host() {
        let github = change(Forge::GitHub, "acme/api", None);
        let other = ChangeRef { number: 8, ..github.clone() };
        let own = change(Forge::GitLab, "acme/api", Some("gitlab.acme.dev"));
        assert!(worktree_dir(&github) != worktree_dir(&other) && worktree_dir(&github) != worktree_dir(&own));
        let odd = change(Forge::GitHub, "../etc/x y", None);
        let dir = worktree_dir(&odd);
        assert!(!dir.contains('/') && !dir.starts_with('.') && !dir.contains(' '), "{dir}");
    }
}
