//! Finds the external commands PR Craft runs (`gh`, `glab`, `git`, `claude`, an editor) and reads their versions.
//!
//! It lives in forge (L1), not core (L0): core does no I/O, and finding a tool stats files. Every layer above forge reaches it from here.
//! An app launched from Finder has a short `PATH` (no Homebrew), so each lookup also tries the usual install folders.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use crate::bounded;

/// Where Homebrew puts commands on Apple silicon and on Intel.
pub const HOMEBREW: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The oldest `gh` and `glab` PR Craft is tested with (`major.minor`), the versions its fixtures were captured from.
pub const MIN_GH: (u32, u32) = (2, 89);
pub const MIN_GLAB: (u32, u32) = (1, 99);

/// How long `<tool> --version` may take.
const PROBE_LIMIT: Duration = Duration::from_secs(5);

fn is_executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The command `tool` names: on the absolute folders of `path`, else at one of `fallbacks`. A fallback that is a folder is searched
/// for `tool`; any other is the full path of the command.
pub fn find(tool: &str, fallbacks: &[&str], path: Option<&OsStr>) -> Option<PathBuf> {
    let listed = path.into_iter().flat_map(std::env::split_paths).filter(|dir| dir.is_absolute()).map(|dir| dir.join(tool));
    let fallen = fallbacks.iter().map(|f| if Path::new(f).is_dir() { Path::new(f).join(tool) } else { PathBuf::from(f) });
    listed.chain(fallen).find(|candidate| is_executable(candidate))
}

/// What to run for `tool`: where [`find`] puts it on this process's `PATH` and the Homebrew folders, else the bare name (so a
/// missing tool fails to start the way it always did).
pub fn resolve(tool: &str) -> PathBuf {
    find(tool, HOMEBREW, std::env::var_os("PATH").as_deref()).unwrap_or_else(|| PathBuf::from(tool))
}

/// A tool's version as its `--version` prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The first `major.minor.patch` in `line`: `gh version 2.89.0 (2026-…)`, `glab 1.99.0 (…)`, `2.1.3 (Claude Code)`.
pub fn parse_version(line: &str) -> Option<Version> {
    line.split(|c: char| !(c.is_ascii_digit() || c == '.')).find_map(|word| {
        let mut parts = word.split('.').map(|p| p.parse::<u32>().ok());
        Some(Version { major: parts.next()??, minor: parts.next()??, patch: parts.next()?? })
    })
}

/// The version of `tool`, asked once per process (a missing or silent tool is remembered as unknown).
pub fn version(tool: &str) -> Option<Version> {
    static SEEN: LazyLock<Mutex<HashMap<String, Option<Version>>>> = LazyLock::new(Mutex::default);
    let seen = &*SEEN;
    if let Some(known) = seen.lock().unwrap_or_else(PoisonError::into_inner).get(tool) {
        return *known;
    }
    let found = probe(tool);
    seen.lock().unwrap_or_else(PoisonError::into_inner).insert(tool.to_string(), found);
    found
}

fn probe(tool: &str) -> Option<Version> {
    let out = bounded::run(Command::new(resolve(tool)).arg("--version"), PROBE_LIMIT).ok()?;
    parse_version(String::from_utf8_lossy(&out.stdout).lines().next()?)
}

/// The oldest `major.minor` of `tool` PR Craft is tested with; none for tools it only reports.
pub fn minimum(tool: &str) -> Option<(u32, u32)> {
    match tool {
        "gh" => Some(MIN_GH),
        "glab" => Some(MIN_GLAB),
        _ => None,
    }
}

/// What to tell the user when `found` is below `min`, or `None` when it is not.
pub fn too_old(tool: &str, found: Version, min: (u32, u32)) -> Option<String> {
    ((found.major, found.minor) < min)
        .then(|| format!("{tool} {}.{} is older than {}.{}, which PR Craft is tested with", found.major, found.minor, min.0, min.1))
}

/// [`too_old`] for `tool`'s own version and minimum: `Some` only when both are known and the tool is below it.
pub fn old_note(tool: &str) -> Option<String> {
    let min = minimum(tool)?;
    too_old(tool, version(tool)?, min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Tmp {
            let dir = std::env::temp_dir().join(format!("pr-craft-tool-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("dir");
            Tmp(dir)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_tool_is_found_on_an_absolute_folder_of_the_path_and_only_when_it_runs() {
        let tmp = Tmp::new("path");
        let (bin, other) = (tmp.0.join("bin"), tmp.0.join("other"));
        fs::create_dir_all(&bin).expect("dir");
        fs::create_dir_all(&other).expect("dir");
        let tool = bin.join("fake-editor");
        fs::write(&tool, "#!/bin/sh\n").expect("script");
        fs::write(other.join("fake-editor"), "not a program").expect("file");
        let search = std::env::join_paths([other.clone(), bin.clone(), PathBuf::from("relative")]).expect("path");
        assert_eq!(find("fake-editor", &[], Some(&search)), None, "no execute bit, nothing found");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(find("fake-editor", &[], Some(&search)), Some(tool.clone()));
        assert_eq!(find("fake-editor", &[tool.to_str().expect("utf8")], None), Some(tool), "a fallback is used when the path has none");
        assert_eq!(find("fake-editor", &[], None), None);
    }

    #[test]
    fn with_an_empty_path_a_tool_is_found_in_a_fallback_folder_when_it_runs() {
        let tmp = Tmp::new("fallback");
        let tool = tmp.0.join("gh");
        fs::write(&tool, "#!/bin/sh\n").expect("script");
        let dir = tmp.0.to_str().expect("utf8");
        let empty = OsStr::new("");
        assert_eq!(find("gh", &[dir], Some(empty)), None, "no execute bit, nothing found");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(find("gh", &[dir], Some(empty)), Some(tool));
    }

    #[test]
    fn real_first_lines_give_their_version() {
        let v = |major, minor, patch| Some(Version { major, minor, patch });
        assert_eq!(parse_version("gh version 2.89.0 (2026-02-11)"), v(2, 89, 0));
        assert_eq!(parse_version("glab 1.99.0 (2026-03-01)"), v(1, 99, 0));
        assert_eq!(parse_version("2.1.3 (Claude Code)"), v(2, 1, 3));
        assert_eq!(parse_version("git version 2.50.1 (Apple Git-155)"), v(2, 50, 1));
        assert_eq!(parse_version("no numbers"), None);
    }

    #[test]
    fn a_version_below_the_minimum_is_named_and_one_at_it_is_not() {
        let old = parse_version("gh version 2.40.1 (2024-01-01)").expect("version");
        assert_eq!(too_old("gh", old, MIN_GH).as_deref(), Some("gh 2.40 is older than 2.89, which PR Craft is tested with"));
        let current = parse_version("gh version 2.89.0").expect("version");
        assert_eq!(too_old("gh", current, MIN_GH), None);
        assert_eq!(minimum("claude"), None);
    }
}
