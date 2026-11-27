//! `palette.query`: what the command palette lists for a line of text, with the saved keys and, unless asked not to, the inbox.

use pr_craft_core::Change;
use pr_craft_core::palette::{Entry, Screen};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use pr_craft_core::palette::search;

use crate::{CommandError, InboxSource, SourceState, inbox_list, keys, params};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    text: String,
    #[serde(default)]
    screen: Option<Screen>,
    /// Read the inbox for the Changes group. The default is yes; a caller that already has the changes says no.
    #[serde(default = "yes")]
    changes: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct Found {
    pub entries: Vec<Entry>,
}

/// The changes the sources listed. A source that is paused or failed lists none: the palette shows what is known.
fn listed(sources: Vec<InboxSource>) -> Vec<Change> {
    sources
        .into_iter()
        .flat_map(|source| match source.state {
            SourceState::Listed { changes, .. } => changes,
            SourceState::Paused { .. } | SourceState::Failed { .. } => Vec::new(),
        })
        .collect()
}

/// `palette.query {text, screen?, changes?}`. Blocks on the forge CLIs when it reads the inbox.
pub fn query(value: &Value) -> Result<Found, CommandError> {
    let p: Query = params(value)?;
    let bindings = keys::bindings()?;
    let inbox = if p.changes { listed(inbox_list()) } else { Vec::new() };
    Ok(Found { entries: search(&p.text, p.screen, &bindings, &inbox) })
}

#[cfg(test)]
mod tests {
    use pr_craft_core::palette::UI_COMMANDS;
    use pr_craft_core::{Forge, Utc};

    use super::*;
    use crate::COMMANDS;

    fn change(number: u64) -> Change {
        Change {
            forge: Forge::GitHub,
            repo: "acme/api".into(),
            number,
            title: "Retry webhooks".into(),
            author: "nina.k".into(),
            url: format!("https://example.test/acme/api/{number}"),
            host: None,
            updated_at: Utc::now(),
            draft: false,
            stack: None,
        }
    }

    #[test]
    fn every_command_the_palette_offers_exists() {
        for command in UI_COMMANDS {
            assert!(COMMANDS.contains(&command.id), "{} is not a command", command.id);
        }
    }

    #[test]
    fn only_a_source_that_listed_gives_changes() {
        let source = |forge, state| InboxSource { forge, cli: forge.cli(), state };
        let sources = vec![
            source(
                Forge::GitHub,
                SourceState::Listed {
                    changes: vec![change(1), change(2)],
                    truncated: false,
                    stacks_error: None,
                    hosts: Vec::new(),
                    signed_out: Vec::new(),
                },
            ),
            source(Forge::GitLab, SourceState::Failed { error: "offline".into() }),
        ];
        assert_eq!(listed(sources).len(), 2);
    }
}
