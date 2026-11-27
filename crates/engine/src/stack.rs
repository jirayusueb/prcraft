//! Stack commands: the layers a change sits among, the diff of all of them, and the inbox grouping.

use std::collections::BTreeSet;

use pr_craft_core::diff::{self, FileDiff};
use pr_craft_core::stack::{StackLayer, place_of, stack_of};
use pr_craft_core::{Change, ChangeRef, Forge};
use pr_craft_forge::stack as forge;
use serde::Serialize;

use crate::CommandError;

/// `stack.whole`: every layer's changes as one read-only diff.
#[derive(Debug, Clone, Serialize)]
pub struct WholeStack {
    /// Bottom first.
    pub layers: Vec<StackLayer>,
    /// The top layer's head against the bottom layer's base branch.
    pub files: Vec<FileDiff>,
    /// Paths in `files` that the forge sent without lines because they are too large; they show no hunks.
    pub omitted: Vec<String>,
    /// Some file is in `omitted`, so the diff is not the whole stack's text.
    pub truncated: bool,
    /// Where the forge shows this comparison, so a file left out can be read there.
    pub web_url: Option<String>,
}

/// `stack.list`: the stack holding `change`, bottom first. A change that is not stacked is a list of one;
/// a change that is not open is a list of none. Each layer's own diff is `review.open` on it: its base is the layer below.
/// Fails when the repository has more open changes than one page holds: the stack may then be incomplete.
pub fn list(change: &ChangeRef) -> Result<Vec<StackLayer>, CommandError> {
    let open = forge::open_layers(change.forge, change.host_name(), &change.repo).map_err(CommandError::Forge)?;
    stack_in(&open, change)
}

fn stack_in(open: &[StackLayer], change: &ChangeRef) -> Result<Vec<StackLayer>, CommandError> {
    if forge::is_full_page(change.forge, open.len()) {
        return Err(CommandError::Invalid(format!(
            "{} has {}+ open changes, which is as many as one read returns, so this stack may be incomplete.",
            change.repo,
            open.len()
        )));
    }
    Ok(stack_of(open, change.number))
}

/// `stack.whole`: blocks on the CLI. Fails for a change that is not in a stack of two or more.
pub fn whole(change: &ChangeRef) -> Result<WholeStack, CommandError> {
    whole_of(change, list(change)?)
}

/// [`whole`] for `layers` already read by [`list`], so a screen that has them does not list open changes again.
pub fn whole_of(change: &ChangeRef, layers: Vec<StackLayer>) -> Result<WholeStack, CommandError> {
    let (Some(bottom), Some(top)) = (layers.first(), layers.last()) else {
        return Err(CommandError::Invalid(format!("{} is not an open change, so it has no stack.", change.reference())));
    };
    if layers.len() < 2 {
        return Err(CommandError::Invalid(format!("{} is not in a stack.", change.reference())));
    }
    if !top.head_in_repo {
        return Err(CommandError::Invalid(
            "The top layer comes from a fork, so the whole stack cannot be compared here; review it layer by layer.".into(),
        ));
    }
    let compare = forge::compare_diff(change, &bottom.base_branch, &top.head_branch).map_err(CommandError::Forge)?;
    let files = diff::parse_unified(&compare.text);
    let truncated = !compare.omitted.is_empty();
    Ok(WholeStack { layers, files, omitted: compare.omitted, truncated, web_url: compare.web_url })
}

/// Sets [`Change::stack`] on every change that sits in a stack of two or more open changes. One lookup per
/// repository, several at a time. Returns a sentence when a repository's stacks could not be read in full:
/// its changes stay ungrouped, and the sentence says so.
pub fn attach(forge: Forge, changes: &mut [Change]) -> Option<String> {
    // Two hosts can each hold a `group/repo`, so a repository is its host and its path.
    let repos: Vec<(Option<String>, String)> =
        changes.iter().map(|c| (c.host.clone(), c.repo.clone())).collect::<BTreeSet<_>>().into_iter().collect();
    let mut problems = Vec::new();
    for batch in repos.chunks(8) {
        let found: Vec<_> = std::thread::scope(|scope| {
            let jobs: Vec<_> = batch
                .iter()
                .map(|(host, repo)| scope.spawn(move || forge::open_layers(forge, host.as_deref().unwrap_or(forge.default_host()), repo)))
                .collect();
            jobs.into_iter().map(|job| job.join().unwrap_or_else(|_| Err("the lookup stopped".into()))).collect()
        });
        for ((host, repo), open) in batch.iter().zip(found) {
            match open {
                Ok(open) => {
                    if forge::is_full_page(forge, open.len()) {
                        problems.push(format!("{repo} has more open changes than were read, so its stacks may be incomplete."));
                    }
                    place(changes, host.as_deref(), repo, &open);
                }
                Err(error) => problems.push(format!("Could not read the open changes of {repo} ({error}), so its stacks are not grouped.")),
            }
        }
    }
    (!problems.is_empty()).then(|| problems.join(" "))
}

fn place(changes: &mut [Change], host: Option<&str>, repo: &str, open: &[StackLayer]) {
    for change in changes.iter_mut().filter(|c| c.repo == repo && c.host.as_deref() == host) {
        change.stack = place_of(open, change.number);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(number: u64, head: &str, base: &str) -> StackLayer {
        StackLayer {
            change: ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number, host: None },
            title: String::new(),
            author: String::new(),
            head_branch: head.into(),
            base_branch: base.into(),
            draft: false,
            url: String::new(),
            head_in_repo: true,
        }
    }

    #[test]
    fn a_full_page_of_open_changes_is_an_error_not_a_stack() {
        // Unrelated open changes fill a page; change 1 sits among them and its stack may continue off the page.
        let limit = forge::layer_limit(Forge::GitHub) as u64;
        let mut open: Vec<StackLayer> = (3..limit + 2).map(|n| layer(n, &format!("b{n}"), "main")).collect();
        open.push(layer(1, "a", "main"));
        let change = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 1, host: None };
        let error = stack_in(&open, &change).expect_err("truncated listing");
        assert!(error.to_string().contains("incomplete"), "{error}");
        assert!(!error.to_string().contains("not an open change"));
    }

    #[test]
    fn a_whole_stack_whose_top_is_a_fork_is_an_error() {
        let mut top = layer(2, "b", "a");
        top.head_in_repo = false;
        let change = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 2, host: None };
        let error = whole_of(&change, vec![layer(1, "a", "main"), top]).expect_err("fork top");
        assert!(error.to_string().contains("fork"), "{error}");
    }

    #[test]
    fn a_short_listing_finds_the_stack_or_nothing() {
        let open = [layer(1, "a", "main"), layer(2, "b", "a")];
        let find = |number| stack_in(&open, &ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number, host: None });
        assert_eq!(find(2).map(|l| l.len()).ok(), Some(2));
        assert_eq!(find(77).map(|l| l.len()).ok(), Some(0));
    }
}
