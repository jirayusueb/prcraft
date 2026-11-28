//! Workspace tooling: `cargo xtask <command>`.

mod bundle;
mod layers;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  ci       fmt --check, clippy -D warnings, check of the render tests, test, doctest, layers (stops at the first failure)
  test     workspace tests (cargo nextest when installed, else cargo test), then doctests
  bundle   build dist/PR Craft.app (ad-hoc signed), its zip and SHA256SUMS.txt; --universal adds an Intel build
  release  refuse unless the live ledger and the weekly macOS build allow it, bundle, then print (never run) the gh release command
  layers   enforce the crate dependency layering (AGENTS.md \"This repository\")
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ci") => ci(),
        Some("test") => test().and_then(|()| doctest()),
        Some("layers") => check_layers(),
        Some("bundle") => bundle::bundle(&args[1..]),
        Some("release") => bundle::release(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

fn cargo(args: &[&str]) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root()).args(args);
    c
}

fn run(args: &[&str]) -> Result<(), String> {
    let what = format!("cargo {}", args.join(" "));
    eprintln!("$ {what}");
    let status = cargo(args).status().map_err(|e| format!("{what}: failed to spawn: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{what}: exited with {status}")) }
}

fn check_layers() -> Result<(), String> {
    let out = cargo(&["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    let meta = serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: bad JSON: {e}"))?;
    let crates = layers::from_metadata(&meta)?;
    let violations = layers::check(&crates);
    if violations.is_empty() {
        println!("layers: {} crates, no violations", crates.len());
        return Ok(());
    }
    for v in &violations {
        println!("  - {v}");
    }
    Err(format!("{} layering violation(s)", violations.len()))
}

/// Examples are left out so they aren't linked; clippy `--all-targets` still compiles them.
const TEST_TARGETS: [&str; 4] = ["--workspace", "--lib", "--bins", "--tests"];

fn test() -> Result<(), String> {
    let nextest = cargo(&["nextest", "--version"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    let runner: &[&str] = if nextest { &["nextest", "run"] } else { &["test"] };
    run(&[runner, &TEST_TARGETS].concat())
}

/// nextest does not run doctests.
fn doctest() -> Result<(), String> {
    run(&["test", "--workspace", "--doc"])
}

fn ci() -> Result<(), String> {
    run(&["fmt", "--all", "--", "--check"])?;
    run(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])?;
    // The render tests are behind a feature CI does not run; they must still compile.
    run(&["check", "-p", "pr-craft-ui-egui", "--features", "renders", "--tests"])?;
    test()?;
    doctest()?;
    check_layers()
}
