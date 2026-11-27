//! `gh pr checkout` and `glab mr checkout`, run inside a clone the user chose. They switch that clone's branch, so the engine
//! asks this only after it has checked the clone is safe to switch.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use pr_craft_core::checkout::valid_repo;
use pr_craft_core::{ChangeRef, Forge};

use crate::bounded;
use crate::command;

/// A checkout fetches the branch, so it may take a while on a slow link; a stalled one is stopped after this.
const LIMIT: Duration = Duration::from_secs(180);

/// Environment that turns git's hooks off for one command: `core.hooksPath=/dev/null`, set through git's own environment config so
/// it beats the clone's. A branch can add a hook file (Husky keeps them in the repository, under a relative `core.hooksPath`), and
/// a checkout would run it with the user's rights; with this none runs, the user's own included (git-lfs's `post-checkout`).
/// It is added after any `GIT_CONFIG_*` entries the environment already has. Git before 2.31 ignores it: see
/// [`pr_craft_core::checkout::hooks_refusal`].
pub fn no_hooks() -> Option<[(String, String); 3]> {
    no_hooks_after(std::env::var("GIT_CONFIG_COUNT").ok().as_deref())
}

/// Why no hook-free command can be made when `GIT_CONFIG_COUNT` is already as large as it can be.
pub const NO_ROOM_FOR_HOOKS: &str = "GIT_CONFIG_COUNT in this environment is too large to add the setting that keeps a hook the change brings from running, so PR Craft will not run git or the forge CLI here.";

/// [`no_hooks`] after `count` entries (the text of `GIT_CONFIG_COUNT`, when set).
/// `None` when the count is so large that one more would wrap: the entry cannot be added, and no hook-free command can be made.
pub fn no_hooks_after(count: Option<&str>) -> Option<[(String, String); 3]> {
    let next = count.and_then(|text| text.trim().parse::<u32>().ok()).unwrap_or(0);
    let total = next.checked_add(1)?;
    Some([
        ("GIT_CONFIG_COUNT".into(), total.to_string()),
        (format!("GIT_CONFIG_KEY_{next}"), "core.hooksPath".into()),
        (format!("GIT_CONFIG_VALUE_{next}"), "/dev/null".into()),
    ])
}

/// The arguments after the CLI's name. The repository goes in one `--repo=` argument, so a name from a forge can never be read
/// as a flag of its own; the number is a number.
pub fn checkout_args(change: &ChangeRef) -> Vec<String> {
    let sub = match change.forge {
        Forge::GitHub => "pr",
        Forge::GitLab => "mr",
    };
    vec![sub.into(), "checkout".into(), change.number.to_string(), format!("--repo={}", change.repo)]
}

/// The command as it will run in `dir`, pinned to the host the clone was matched on: `GH_HOST` for gh, `GITLAB_HOST` for glab, so
/// a different default host in the user's environment cannot send the checkout to another server's repository of the same name.
pub fn checkout_command(change: &ChangeRef, dir: &Path) -> Result<Command, String> {
    let hooks_off = no_hooks().ok_or(NO_ROOM_FOR_HOOKS)?;
    let mut command = command(change.host_name(), change.forge.cli());
    if change.forge == Forge::GitHub {
        command.env("GH_HOST", change.host_name());
    }
    command.envs(hooks_off).args(checkout_args(change)).current_dir(dir).env("GH_PROMPT_DISABLED", "1").env("GIT_TERMINAL_PROMPT", "0");
    Ok(command)
}

/// Checks the change out in the clone at `dir`. Never answers a prompt: a login or a choice the CLI wants fails the call.
pub fn checkout_in(change: &ChangeRef, dir: &Path) -> Result<(), String> {
    if !valid_repo(&change.repo) {
        return Err(format!("`{}` is not a repository path PR Craft will pass to {}", change.repo, change.forge.cli()));
    }
    let cli = change.forge.cli();
    let out = bounded::run(&mut checkout_command(change, dir)?, LIMIT).map_err(|e| format!("{cli} could not run: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("`{cli} {}` failed: {}", checkout_args(change).join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn change(forge: Forge, repo: &str, host: Option<&str>) -> ChangeRef {
        ChangeRef { forge, repo: repo.into(), number: 482, host: host.map(str::to_string) }
    }

    fn env<'a>(command: &'a Command, name: &str) -> Option<&'a OsStr> {
        command.get_envs().find(|(key, _)| *key == OsStr::new(name)).and_then(|(_, value)| value)
    }

    #[test]
    fn each_forge_gets_its_own_checkout_with_the_repository_in_one_argument() {
        assert_eq!(checkout_args(&change(Forge::GitHub, "acme/api", None)), ["pr", "checkout", "482", "--repo=acme/api"]);
        assert_eq!(checkout_args(&change(Forge::GitLab, "grp/sub/proj", None)), ["mr", "checkout", "482", "--repo=grp/sub/proj"]);
    }

    #[test]
    fn the_checkout_is_pinned_to_the_host_the_clone_was_matched_on() {
        let dir = Path::new("/work");
        let github = checkout_command(&change(Forge::GitHub, "acme/api", Some("ghe.acme.dev")), dir).expect("a command");
        assert_eq!(env(&github, "GH_HOST"), Some(OsStr::new("ghe.acme.dev")));
        let default = checkout_command(&change(Forge::GitHub, "acme/api", None), dir).expect("a command");
        assert_eq!(env(&default, "GH_HOST"), Some(OsStr::new("github.com")));
        let gitlab = checkout_command(&change(Forge::GitLab, "grp/proj", Some("gitlab.acme.dev")), dir).expect("a command");
        assert_eq!(env(&gitlab, "GITLAB_HOST"), Some(OsStr::new("gitlab.acme.dev")));
        assert_eq!(github.get_current_dir(), Some(dir));
        for (name, value) in no_hooks().expect("room for one more entry") {
            assert_eq!(env(&github, &name), Some(OsStr::new(&value)), "{name}: a hook a branch brings must not run for the switch");
            assert_eq!(env(&gitlab, &name), Some(OsStr::new(&value)));
        }
    }

    #[test]
    fn the_hooks_entry_goes_after_the_config_entries_the_environment_already_has() {
        let none: Vec<_> = no_hooks_after(None).expect("room").into_iter().map(|(k, _)| k).collect();
        assert_eq!(none, ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"]);
        let two = no_hooks_after(Some("2")).expect("room");
        assert_eq!(two[0], ("GIT_CONFIG_COUNT".to_string(), "3".to_string()), "the user's two entries stay, ours is the third");
        assert_eq!((two[1].0.as_str(), two[1].1.as_str()), ("GIT_CONFIG_KEY_2", "core.hooksPath"));
        assert_eq!(no_hooks_after(Some("junk")).expect("room")[1].0, "GIT_CONFIG_KEY_0");
        assert_eq!(no_hooks_after(Some("4294967294")).expect("the last free index")[0].1, "4294967295");
        assert_eq!(no_hooks_after(Some("4294967295")), None, "one more would wrap to 0 and leave hooks on: no command is made");
    }

    #[test]
    fn a_repository_name_that_could_be_a_flag_is_never_run() {
        let evil = change(Forge::GitHub, "--exec=sh/x", None);
        let error = checkout_in(&evil, Path::new("/nonexistent")).expect_err("refused before any process starts");
        assert!(error.contains("not a repository path"), "{error}");
    }
}
