//! `cargo xtask bundle` makes the zip a teammate installs; `cargo xtask release` checks the gates and prints the command that
//! publishes it. Nothing here publishes (AGENTS.md rule 4): the maintainer runs the printed command.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pr_craft_engine::update::zip_name;

use crate::{cargo, root};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const APP: &str = "PR Craft.app";
const MAC_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
/// The GitHub rows that must be `pass` before a Release (the ledger's own wording, docs/live-ledger.md).
const REQUIRED: [&str; 3] = ["review.submit", "thread.reply", "thread.resolve"];

/// The Info.plist template with the version and bundle id filled in.
pub fn render_plist(template: &str, version: &str, app_id: &str) -> String {
    template.replace("{{VERSION}}", version).replace("{{APP_ID}}", app_id)
}

/// `APP_ID` as `crates/ui-egui/src/lib.rs` declares it, so the bundle id has one source.
pub fn app_id(lib_rs: &str) -> Option<&str> {
    let rest = lib_rs.split("pub const APP_ID: &str = \"").nth(1)?;
    rest.split('"').next()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Gate {
    /// A GitLab row is `blocked`: the notes must say so.
    pub gitlab_blocked: bool,
}

struct Row<'a> {
    command: &'a str,
    forge: &'a str,
    result: &'a str,
}

fn rows(ledger: &str) -> Vec<Row<'_>> {
    ledger
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .filter_map(|line| {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
            // Command, Forge, Host, Milestone, Sandbox change, Date, Result, How run. The header and `---` rows are no data.
            (cells.len() == 8 && cells[0] != "Command" && !cells[0].starts_with('-')).then(|| Row {
                command: cells[0].trim_matches('`'),
                forge: cells[1],
                result: cells[6],
            })
        })
        .collect()
}

/// Whether the ledger lets a Release through: the GitHub rows of [`REQUIRED`] are `pass` and no row is `fail`.
pub fn gate(ledger: &str) -> Result<Gate, Vec<String>> {
    let rows = rows(ledger);
    let mut refused = Vec::new();
    for command in REQUIRED {
        match rows.iter().find(|r| r.forge == "GitHub" && r.command == command) {
            Some(row) if row.result == "pass" => {}
            Some(row) if row.result.is_empty() => refused.push(format!("GitHub {command}: not run yet")),
            Some(row) => refused.push(format!("GitHub {command}: {}", row.result)),
            None => refused.push(format!("GitHub {command}: no row in docs/live-ledger.md")),
        }
    }
    for row in rows.iter().filter(|r| r.result == "fail") {
        refused.push(format!("{} {}: fail", row.forge, row.command));
    }
    if !refused.is_empty() {
        return Err(refused);
    }
    Ok(Gate { gitlab_blocked: rows.iter().any(|r| r.forge == "GitLab" && r.result.starts_with("blocked")) })
}

/// What `gh run list … --json conclusion` says of the weekly macOS build: green, or why not.
pub fn weekly_verdict(listed: Result<&str, &str>) -> Result<(), String> {
    let out = match listed {
        Ok(out) => out,
        Err(e) if e.contains("could not find any workflows") || e.contains("HTTP 404") => {
            return Err("the weekly macOS build does not exist yet (no weekly-macos.yml workflow on GitHub)".into());
        }
        Err(e) => return Err(format!("could not ask GitHub for the weekly macOS build: {}", e.trim())),
    };
    let runs: Vec<serde_json::Value> = serde_json::from_str(out).map_err(|e| format!("gh run list answered something else than JSON: {e}"))?;
    match runs.first().and_then(|r| r["conclusion"].as_str()) {
        Some("success") => Ok(()),
        Some(other) => Err(format!("the latest weekly macOS build on main ended with `{other}`, not `success`")),
        None => Err("the weekly macOS build has no finished run on main yet".into()),
    }
}

pub fn release_notes(gate: &Gate) -> String {
    let mut notes = format!("PR Craft {VERSION}. Install: docs/install.md. The app is ad-hoc signed, not notarized.\n");
    if gate.gitlab_blocked {
        notes.push_str("\nGitLab writes not verified.\n");
    }
    notes
}

pub fn release(args: &[String]) -> Result<(), String> {
    let ledger = fs::read_to_string(root().join("docs/live-ledger.md")).map_err(|e| format!("docs/live-ledger.md: {e}"))?;
    let gate = gate(&ledger).map_err(|rows| format!("the live ledger does not allow a release yet:\n  - {}", rows.join("\n  - ")))?;
    let listed = Command::new("gh")
        .args(["run", "list", "--workflow", "weekly-macos.yml", "--branch", "main", "--limit", "1", "--json", "conclusion"])
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    let (out, err) = (String::from_utf8_lossy(&listed.stdout), String::from_utf8_lossy(&listed.stderr));
    weekly_verdict(if listed.status.success() { Ok(&out) } else { Err(&err) })?;
    bundle(args)?;
    let notes = release_notes(&gate);
    let notes_file = root().join("dist/RELEASE-NOTES.md");
    fs::write(&notes_file, &notes).map_err(|e| format!("{}: {e}", notes_file.display()))?;
    println!("\nRelease notes (dist/RELEASE-NOTES.md):\n{notes}\nNothing was published. To publish, run:\n");
    println!("gh release create v{VERSION} dist/*.zip dist/SHA256SUMS.txt --notes-file dist/RELEASE-NOTES.md --title \"PR Craft v{VERSION}\"");
    Ok(())
}

fn tool(name: &str, args: &[&str], dir: &Path) -> Result<String, String> {
    eprintln!("$ {name} {}", args.join(" "));
    let out = Command::new(name).args(args).current_dir(dir).output().map_err(|e| format!("{name}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{name} {}: {}\n{}", args.join(" "), out.status, String::from_utf8_lossy(&out.stderr)))
    }
}

fn target_dir() -> PathBuf {
    root().join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
}

fn build(target: &str, commit: &str) -> Result<PathBuf, String> {
    let args = ["build", "--release", "-p", "pr-craft", "--target", target];
    eprintln!("$ PR_CRAFT_COMMIT={commit} cargo {}", args.join(" "));
    let status = cargo(&args).env("PR_CRAFT_COMMIT", commit).status().map_err(|e| format!("cargo build: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build for {target}: exited with {status}"));
    }
    Ok(target_dir().join(target).join("release/pr-craft"))
}

/// Lays out the .app around `binary`, signs it ad hoc and zips it next to the others; returns the zip's file name.
fn package(dist: &Path, binary: &Path, plist: &str, arch: &str) -> Result<String, String> {
    let app = dist.join(APP);
    let _ = fs::remove_dir_all(&app);
    let io = |e: std::io::Error| e.to_string();
    fs::create_dir_all(app.join("Contents/MacOS")).map_err(io)?;
    fs::create_dir_all(app.join("Contents/Resources")).map_err(io)?;
    fs::write(app.join("Contents/Info.plist"), plist).map_err(io)?;
    fs::copy(binary, app.join("Contents/MacOS/pr-craft")).map_err(io)?;
    tool("codesign", &["--force", "--deep", "--sign", "-", APP], dist)?;
    let zip = zip_name(VERSION, arch);
    let _ = fs::remove_file(dist.join(&zip));
    tool("ditto", &["-c", "-k", "--keepParent", APP, &zip], dist)?;
    Ok(zip)
}

pub fn bundle(args: &[String]) -> Result<(), String> {
    let universal = match args {
        [] => false,
        [flag] if flag == "--universal" => true,
        _ => return Err("usage: cargo xtask bundle [--universal]".into()),
    };
    let root = root();
    let commit = tool("git", &["rev-parse", "HEAD"], &root)?.trim().to_string();
    let lib_rs = fs::read_to_string(root.join("crates/ui-egui/src/lib.rs")).map_err(|e| format!("crates/ui-egui/src/lib.rs: {e}"))?;
    let id = app_id(&lib_rs).ok_or("crates/ui-egui/src/lib.rs: no `pub const APP_ID` found")?;
    let template = fs::read_to_string(root.join("apps/pr-craft/macos/Info.plist")).map_err(|e| format!("Info.plist template: {e}"))?;
    let plist = render_plist(&template, VERSION, id);

    let dist = root.join("dist");
    fs::create_dir_all(&dist).map_err(|e| e.to_string())?;
    let arm = build(MAC_TARGETS[0], &commit)?;
    let mut zips = vec![package(&dist, &arm, &plist, "arm64")?];
    if universal {
        let intel = build(MAC_TARGETS[1], &commit)?;
        let joined = target_dir().join("universal-pr-craft");
        let (a, i, j) = (arm.to_string_lossy(), intel.to_string_lossy(), joined.to_string_lossy());
        tool("lipo", &["-create", &a, &i, "-output", &j], &root)?;
        zips.push(package(&dist, &joined, &plist, "universal")?);
    }
    let refs: Vec<&str> = zips.iter().map(String::as_str).collect();
    let mut sums = String::new();
    for zip in &refs {
        sums.push_str(&tool("shasum", &["-a", "256", zip], &dist)?);
    }
    fs::write(dist.join("SHA256SUMS.txt"), sums).map_err(|e| e.to_string())?;
    // The app dir holds the last build packaged; verify it before telling the maintainer it is ready.
    tool("codesign", &["--verify", APP], &dist)?;
    println!("bundled {VERSION} ({commit}): {}", refs.iter().map(|z| format!("dist/{z}")).collect::<Vec<_>>().join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: [&str; 3], gitlab: &str) -> String {
        let row = |cmd: &str, forge: &str, result: &str| format!("| `{cmd}` | {forge} | h | M2 | | | {result} | |\n");
        let mut t = String::from(
            "| Command | Forge | Host | Milestone | Sandbox change | Date | Result | How run |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n",
        );
        for (cmd, result) in REQUIRED.iter().zip(rows) {
            t.push_str(&row(cmd, "GitHub", result));
        }
        t.push_str(&row("review.hand_off", "GitHub", ""));
        t.push_str(&row("thread.reply", "GitLab", gitlab));
        t
    }

    #[test]
    fn all_three_github_rows_passing_lets_a_release_through() {
        assert_eq!(gate(&table(["pass", "pass", "pass"], "")), Ok(Gate { gitlab_blocked: false }));
    }

    #[test]
    fn an_empty_github_row_is_named() {
        let refused = gate(&table(["pass", "", "pass"], "")).unwrap_err();
        assert_eq!(refused, ["GitHub thread.reply: not run yet"]);
    }

    #[test]
    fn a_fail_row_refuses_even_when_the_required_rows_pass() {
        let refused = gate(&table(["pass", "pass", "pass"], "fail")).unwrap_err();
        assert_eq!(refused, ["GitLab thread.reply: fail"]);
    }

    #[test]
    fn a_blocked_gitlab_row_passes_and_the_notes_say_so() {
        let ok = gate(&table(["pass", "pass", "pass"], "blocked: no sandbox")).unwrap();
        assert!(ok.gitlab_blocked);
        assert!(release_notes(&ok).contains("GitLab writes not verified"));
        assert!(!release_notes(&Gate { gitlab_blocked: false }).contains("GitLab"));
    }

    #[test]
    fn the_shipped_ledger_refuses_until_rows_are_filled() {
        let header =
            "| Command | Forge | Host | Milestone | Sandbox change | Date | Result | How run |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n";
        assert_eq!(gate(header).unwrap_err().len(), 3, "a ledger with no rows names every required row");
    }

    #[test]
    fn the_weekly_build_must_exist_and_be_green() {
        assert!(weekly_verdict(Ok(r#"[{"conclusion":"success"}]"#)).is_ok());
        assert!(weekly_verdict(Ok(r#"[{"conclusion":"failure"}]"#)).unwrap_err().contains("failure"));
        assert!(weekly_verdict(Ok("[]")).unwrap_err().contains("no finished run"));
        assert!(weekly_verdict(Err("could not find any workflows named weekly-macos.yml")).unwrap_err().contains("does not exist yet"));
    }

    #[test]
    fn the_plist_takes_the_version_and_the_id() {
        let out = render_plist(&fs::read_to_string(root().join("apps/pr-craft/macos/Info.plist")).unwrap(), "1.2.3", "ai.example.app");
        assert!(out.contains("<string>1.2.3</string>") && out.contains("<string>ai.example.app</string>"));
        assert!(!out.contains("{{"));
    }

    #[test]
    fn the_app_id_is_read_from_the_ui_crate() {
        let lib = fs::read_to_string(root().join("crates/ui-egui/src/lib.rs")).unwrap();
        assert_eq!(app_id(&lib), Some("ai.storyteller.prcraft"));
    }
}
