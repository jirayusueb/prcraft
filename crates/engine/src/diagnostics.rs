//! `diagnostics.get`: what a teammate pastes into a report. The text names the app build, macOS, which hosts the forge CLIs are
//! signed in to, the data version and the last log lines. Secrets are masked and the home folder is written as `~`.

use std::process::Command;
use std::time::Duration;

use pr_craft_core::Forge;
use pr_craft_core::redact::redact;
use pr_craft_forge::{Login, bounded, tool};
use serde_json::{Value, json};

use crate::{CommandError, log, store};

/// How many log lines the block carries.
const LOG_LINES: usize = 200;

fn macos() -> String {
    let mut command = Command::new("sw_vers");
    command.arg("-productVersion");
    match bounded::run(&mut command, Duration::from_secs(5)) {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "unknown".into(),
    }
}

/// One line per forge: each host it knows and whether the CLI can act there, or why it cannot act at all.
fn forges() -> Vec<String> {
    Forge::ALL
        .into_iter()
        .map(|forge| {
            let state = match pr_craft_forge::hosts(forge) {
                Ok(hosts) => hosts
                    .iter()
                    .map(|h| format!("{} {}", h.name, if h.signed_in { "signed in" } else { "signed out" }))
                    .collect::<Vec<_>>()
                    .join(", "),
                Err(Login::LoggedOut { .. }) => "signed out".into(),
                Err(Login::CliMissing { .. }) => "CLI missing".into(),
                Err(Login::CliOld { message, .. }) => message,
                Err(Login::Scope { fix }) => format!("token lacks a scope, run `{fix}`"),
                Err(Login::Failed { error }) => format!("failed: {error}"),
                Err(Login::LoggedIn) => "signed in".into(),
            };
            format!("{}: {state}", forge.cli())
        })
        .collect()
}

/// One line per external tool: where it was found and its version.
fn tools() -> Vec<String> {
    ["git", "gh", "glab", "claude"]
        .map(|name| {
            let path = tool::resolve(name);
            let found = path
                .is_file()
                .then(|| format!("{} {}", path.display(), tool::version(name).map_or_else(|| "version unknown".into(), |v| v.to_string())));
            format!("{name}: {}", found.unwrap_or_else(|| "not found".into()))
        })
        .into()
}

/// The text with every secret shape masked and `home` written as `~`.
fn clean(text: &str, home: Option<&str>) -> String {
    let masked = redact(text).text;
    match home {
        Some(home) if !home.is_empty() => masked.replace(home, "~"),
        _ => masked,
    }
}

fn block(version: &str, commit: &str, macos: &str, data_version: Option<u32>, forges: &[String], tools: &[String], log: &[String]) -> String {
    let data = data_version.map_or_else(|| "none yet".to_string(), |v| v.to_string());
    let mut text = format!("PR Craft {version} ({commit})\nmacOS {macos}\ndata version {data}\n");
    for forge in forges.iter().chain(tools) {
        text.push_str(forge);
        text.push('\n');
    }
    text.push_str(&format!("last {} log lines:\n", log.len()));
    for line in log {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// What a report holds, gathered by [`get`].
struct Inputs<'a> {
    version: &'a str,
    commit: &'a str,
    macos: &'a str,
    data_version: Option<u32>,
    forges: &'a [String],
    tools: &'a [String],
    log: &'a [String],
    home: Option<&'a str>,
}

/// The answer of `diagnostics.get` from its inputs: the fields, and `text` ready to paste.
fn assemble(i: &Inputs<'_>) -> Value {
    let text = clean(&block(i.version, i.commit, i.macos, i.data_version, i.forges, i.tools, i.log), i.home);
    json!({ "version": i.version, "commit": i.commit, "macos": i.macos, "data_version": i.data_version, "forges": i.forges, "tools": i.tools, "text": text })
}

pub fn get() -> Result<Value, CommandError> {
    let data_version = store::data_dir().ok().and_then(|dir| store::data_version(&dir));
    let log = log::dir().map(|dir| log::tail_in(dir, LOG_LINES)).unwrap_or_default();
    let home = std::env::var("HOME").ok();
    let inputs = Inputs {
        version: env!("CARGO_PKG_VERSION"),
        commit: option_env!("PR_CRAFT_COMMIT").unwrap_or("dev"),
        macos: &macos(),
        data_version,
        forges: &forges(),
        tools: &tools(),
        log: &log,
        home: home.as_deref(),
    };
    Ok(assemble(&inputs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_names_the_build_and_hides_the_home_folder_and_secrets() {
        let token = format!("ghp_{}", "a1B2".repeat(9));
        let log = vec![format!("2026-11-28T10:00:00Z review.open repo=/Users/ada/work {token} ok 4ms")];
        let forges = ["gh: github.com signed in".to_string()];
        let tools = ["gh: /opt/homebrew/bin/gh 2.89.0".to_string()];
        let inputs = Inputs {
            version: "0.1.0",
            commit: "abc123",
            macos: "26.1",
            data_version: Some(1),
            forges: &forges,
            tools: &tools,
            log: &log,
            home: Some("/Users/ada"),
        };
        let answer = assemble(&inputs);
        let text = answer["text"].as_str().expect("text");
        assert!(text.contains("PR Craft 0.1.0 (abc123)") && text.contains("data version 1"), "{text}");
        assert!(text.contains("gh: /opt/homebrew/bin/gh 2.89.0"), "{text}");
        assert!(text.contains("~/work"), "{text}");
        assert!(!text.contains("/Users/ada"), "{text}");
        assert!(!text.contains(&token), "{text}");
        assert_eq!(redact(text).found, 0, "nothing redact would mask is left: {text}");
        assert_eq!(answer["version"], "0.1.0");
    }
}
