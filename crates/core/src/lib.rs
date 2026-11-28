//! PR Craft's words for the forges it reads, shared by every layer. No I/O.

pub mod agent;
pub mod checkout;
pub mod commits;
pub mod context;
pub mod diff;
pub mod diff_view;
pub mod editor;
pub mod extra;
pub mod filter;
pub mod gitref;
pub mod keys;
pub mod mine;
pub mod palette;
pub mod redact;
pub mod review;
pub mod run;
pub mod settings;
pub mod share;
pub mod stack;
pub mod svg;
pub mod thread;
pub mod turn;
pub mod view;

/// The time types the shared structs use, so the engine and forge layers name them without a second dependency.
pub use chrono::{DateTime, Local, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

/// A code forge PR Craft reads changes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Forge {
    GitHub,
    GitLab,
}

impl Forge {
    pub const ALL: [Forge; 2] = [Forge::GitHub, Forge::GitLab];

    pub fn name(self) -> &'static str {
        match self {
            Forge::GitHub => "GitHub",
            Forge::GitLab => "GitLab",
        }
    }

    /// The CLI whose login PR Craft reuses (PRODUCT.md: the app never asks for a token).
    pub fn cli(self) -> &'static str {
        match self {
            Forge::GitHub => "gh",
            Forge::GitLab => "glab",
        }
    }

    /// What sits between a repository and a change number: `acme/api #482`, `infra-modules !88`.
    pub fn number_sigil(self) -> char {
        match self {
            Forge::GitHub => '#',
            Forge::GitLab => '!',
        }
    }

    /// The host the forge's CLI talks to when a change names none.
    pub fn default_host(self) -> &'static str {
        match self {
            Forge::GitHub => "github.com",
            Forge::GitLab => "gitlab.com",
        }
    }

    /// What a change stores for `host`: nothing for the default host, so keys written before hosts existed still match.
    pub fn host_field(self, host: &str) -> Option<String> {
        (!host.eq_ignore_ascii_case(self.default_host())).then(|| host.to_ascii_lowercase())
    }
}

/// One open pull request (GitHub) or merge request (GitLab).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub forge: Forge,
    /// `owner/name` on GitHub, the full project path on GitLab.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    /// A self-hosted server such as `gitlab.acme.dev`; `None` is [`Forge::default_host`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub draft: bool,
    /// Set by the engine when this change sits in a stack of two or more open changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<stack::StackPlace>,
}

impl Change {
    pub fn reference(&self) -> String {
        self.change_ref().reference()
    }

    pub fn change_ref(&self) -> ChangeRef {
        ChangeRef { forge: self.forge, repo: self.repo.clone(), number: self.number, host: self.host.clone() }
    }
}

/// Names one change on one forge; every review command takes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangeRef {
    pub forge: Forge,
    pub repo: String,
    pub number: u64,
    /// A self-hosted server; `None` is [`Forge::default_host`]. Old saved state has no host and still loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl ChangeRef {
    pub fn reference(&self) -> String {
        format!("{} {}{}", self.repo, self.forge.number_sigil(), self.number)
    }

    /// The server every CLI call for this change goes to.
    pub fn host_name(&self) -> &str {
        self.host.as_deref().unwrap_or(self.forge.default_host())
    }
}
