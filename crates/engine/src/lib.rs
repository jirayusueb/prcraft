//! Commands: the one way the UI, the CLI and automation read or change state (AGENTS.md rule 14).

pub mod ai;
pub mod banner;
pub mod commits;
pub mod diagnostics;
pub mod keys;
pub mod local;
pub mod log;
pub mod mine;
pub mod palette;
pub mod quota;
pub mod review;
mod runner;
pub mod settings;
pub mod share;
pub mod stack;
mod store;
pub mod tally;
pub mod thread;
pub mod turn;
pub mod update;
pub mod view;

use pr_craft_core::review::Verdict;
use pr_craft_core::view::FileView;
use pr_craft_core::{Change, ChangeRef, DateTime, Forge, Utc};
use pr_craft_forge::{FailKind, ForgeError, Login};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BASE: &[&str] = &[
    "forge.status",
    "diagnostics.get",
    "inbox.list",
    "review.open",
    "review.local",
    "review.drafts_all",
    "review.mark_viewed",
    "review.filter",
    "draft.add",
    "draft.edit",
    "draft.delete",
    "review.compose",
    "review.submit",
    "review.markdown",
    "stack.list",
    "stack.whole",
    "inbox.back_to_you",
    "review.since_last",
    "review.hand_off",
    "notify.list",
    "notify.pause",
    "notify.seen",
    "notify.banners",
    "review.blob",
    "review.set_view",
    "review.views",
    "review.set_layout",
    "review.expand",
    "share.render",
    "share.save",
    "share.copy",
    "quota.status",
    "settings.get",
    "settings.set",
    "keys.list",
    "keys.rebind",
    "keys.reset",
    "palette.query",
    "thread.list",
    "thread.reply",
    "thread.resolve",
    "review.commits",
    "review.range",
    "checkout.status",
    "checkout.worktree",
    "checkout.here",
    "open.editor",
];

/// `parts` end to end, as one array, so the AI and Your-changes commands are listed once (in [`ai::COMMANDS`] and
/// [`mine::COMMANDS`]) and still appear in [`COMMANDS`].
const fn join<const N: usize>(parts: &[&[&'static str]]) -> [&'static str; N] {
    let mut out = [""; N];
    let (mut at, mut part) = (0, 0);
    while part < parts.len() {
        let mut i = 0;
        while i < parts[part].len() {
            out[at] = parts[part][i];
            at += 1;
            i += 1;
        }
        part += 1;
    }
    out
}

/// Every command id [`run`] takes.
pub const COMMANDS: &[&str] =
    &join::<{ BASE.len() + ai::COMMANDS.len() + mine::COMMANDS.len() + update::COMMANDS.len() + tally::COMMANDS.len() }>(&[
        BASE,
        &ai::COMMANDS,
        &mine::COMMANDS,
        &update::COMMANDS,
        &tally::COMMANDS,
    ]);

/// Which way a failed read or write of the files on this Mac was going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    Read,
    Write,
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("unknown command `{0}` (commands: {list})", list = COMMANDS.join(", "))]
    Unknown(String),
    #[error("could not encode the result: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("bad params: {0}")]
    Params(serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    /// The forge or its CLI refused or failed; the message says what stays on this Mac.
    #[error("{0}")]
    Forge(#[from] ForgeError),
    /// Reading or writing the review files on this Mac failed.
    #[error("{message}")]
    Store { kind: StoreKind, message: String },
    /// Asking GitHub for a release, or downloading it, failed.
    #[error("{0}")]
    Update(String),
}

impl CommandError {
    pub(crate) fn read(message: impl Into<String>) -> Self {
        CommandError::Store { kind: StoreKind::Read, message: message.into() }
    }

    pub(crate) fn write(message: impl Into<String>) -> Self {
        CommandError::Store { kind: StoreKind::Write, message: message.into() }
    }

    /// The stable code a report quotes, so a failure is told from its text alone.
    pub fn code(&self) -> &'static str {
        match self {
            CommandError::Unknown(_) => "E-UNKNOWN",
            CommandError::Encode(_) => "E-ENCODE",
            CommandError::Params(_) => "E-PARAMS",
            CommandError::Invalid(_) => "E-INVALID",
            CommandError::Forge(error) => match error.kind {
                FailKind::Auth => "E-FORGE-AUTH",
                FailKind::Scope => "E-FORGE-SCOPE",
                FailKind::RateLimit => "E-FORGE-RATE",
                FailKind::Network => "E-FORGE-NET",
                FailKind::CliMissing => "E-FORGE-CLI",
                FailKind::CliOld => "E-FORGE-CLI-OLD",
                FailKind::Timeout => "E-FORGE-TIMEOUT",
                FailKind::Refused => "E-FORGE-REFUSED",
                FailKind::Other => "E-FORGE-OTHER",
            },
            CommandError::Store { kind: StoreKind::Read, .. } => "E-STORE-READ",
            CommandError::Store { kind: StoreKind::Write, .. } => "E-STORE-WRITE",
            CommandError::Update(_) => "E-UPDATE",
        }
    }
}

#[derive(Deserialize)]
struct On {
    change: ChangeRef,
}

#[derive(Deserialize)]
struct Viewed {
    change: ChangeRef,
    path: String,
    hash: Option<u64>,
}

#[derive(Deserialize)]
struct Filter {
    forge: Forge,
    /// A self-hosted server; omitted is the forge's default host.
    #[serde(default)]
    host: Option<String>,
    repo: String,
    patterns: Vec<String>,
}

#[derive(Deserialize)]
struct AddDraft {
    change: ChangeRef,
    #[serde(flatten)]
    draft: review::NewDraft,
}

#[derive(Deserialize)]
struct EditDraft {
    change: ChangeRef,
    id: u64,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct Compose {
    change: ChangeRef,
    verdict: Verdict,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct Markdown {
    change: ChangeRef,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct HandOffTo {
    change: ChangeRef,
    #[serde(default)]
    to: String,
}

#[derive(Deserialize)]
struct PauseFor {
    /// Pause until this time; without it, until `resume`.
    until: Option<DateTime<Utc>>,
    #[serde(default)]
    resume: bool,
    /// Flip whatever the stored pause is now: end it, or start one until resumed.
    #[serde(default)]
    toggle: bool,
}

#[derive(Deserialize)]
struct Seen {
    ids: Vec<String>,
}

#[derive(Deserialize)]
struct BlobOf {
    change: ChangeRef,
    path: String,
    side: view::Side,
}

#[derive(Deserialize)]
struct SetView {
    extension: String,
    view: FileView,
}

#[derive(Deserialize)]
struct ReplyTo {
    change: ChangeRef,
    thread: String,
    body: String,
}

#[derive(Deserialize)]
struct ResolveThread {
    change: ChangeRef,
    thread: String,
    /// Reopens the thread when false; resolves it when absent.
    resolved: Option<bool>,
}

#[derive(Deserialize)]
struct RangeOf {
    change: ChangeRef,
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct SetLayout {
    forge: Forge,
    #[serde(default)]
    host: Option<String>,
    repo: String,
    options: pr_craft_core::diff_view::LayoutOptions,
}

#[derive(Deserialize)]
struct Expand {
    change: ChangeRef,
    path: String,
    side: view::Side,
    from: u32,
    to: u32,
}

#[derive(Deserialize)]
struct Submit {
    change: ChangeRef,
    /// Say yes to posting again after a post that ran out of time (`post_unknown` in `review.local`): the reader checked the forge.
    #[serde(default)]
    confirm_unknown: bool,
    /// Say yes to posting drafts whose lines changed since they were written.
    #[serde(default)]
    confirm_outdated: bool,
}

#[derive(Deserialize)]
struct CheckoutHere {
    change: ChangeRef,
    /// Must be `true`: the reader has seen the clone and the branch it switches to.
    #[serde(default)]
    confirm: bool,
    /// The branch the confirm step named.
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Deserialize)]
struct OpenEditor {
    change: ChangeRef,
    editor: pr_craft_core::editor::Editor,
    /// Relative to the checkout; Terminal opens the checkout and needs none.
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
}

fn params<T: DeserializeOwned>(value: &Value) -> Result<T, CommandError> {
    T::deserialize(value).map_err(CommandError::Params)
}

#[derive(Debug, Clone, Serialize)]
pub struct ForgeStatus {
    pub forge: Forge,
    pub cli: &'static str,
    pub login: Login,
}

/// Runs command `id` with its JSON params. Review commands take `{"change": {"forge", "repo", "number"}, ...}`.
pub fn run(id: &str, value: &Value) -> Result<Value, CommandError> {
    let started = std::time::Instant::now();
    let result = dispatch(id, value);
    log::record(id, value, &result, started.elapsed());
    result
}

fn dispatch(id: &str, value: &Value) -> Result<Value, CommandError> {
    match id {
        "forge.status" => Ok(serde_json::to_value(forge_status())?),
        "diagnostics.get" => diagnostics::get(),
        "inbox.list" => match value.get("forge") {
            None => Ok(serde_json::to_value(inbox_list())?),
            Some(forge) => {
                let forge: Forge = serde_json::from_value(forge.clone()).map_err(CommandError::Params)?;
                Ok(serde_json::to_value([inbox_source(forge)])?)
            }
        },
        "review.open" => Ok(serde_json::to_value(review::open(&params::<On>(value)?.change)?)?),
        "review.local" => Ok(serde_json::to_value(review::local(&params::<On>(value)?.change)?)?),
        "review.drafts_all" => Ok(serde_json::to_value(review::drafts_all()?)?),
        "review.mark_viewed" => {
            let p: Viewed = params(value)?;
            Ok(serde_json::to_value(review::mark_viewed(&p.change, &p.path, p.hash)?)?)
        }
        "review.filter" => {
            let p: Filter = params(value)?;
            let host = p.host.as_deref().and_then(|h| p.forge.host_field(h));
            review::save_filter(p.forge, host.as_deref(), &p.repo, p.patterns)?;
            Ok(Value::Null)
        }
        "draft.add" => {
            let p: AddDraft = params(value)?;
            Ok(serde_json::to_value(review::draft_add(&p.change, p.draft)?)?)
        }
        "draft.edit" => {
            let p: EditDraft = params(value)?;
            Ok(serde_json::to_value(review::draft_edit(&p.change, p.id, p.body)?)?)
        }
        "draft.delete" => {
            let p: EditDraft = params(value)?;
            Ok(serde_json::to_value(review::draft_delete(&p.change, p.id)?)?)
        }
        "review.compose" => {
            let p: Compose = params(value)?;
            Ok(serde_json::to_value(review::compose(&p.change, p.verdict, p.body)?)?)
        }
        "review.submit" => {
            let p: Submit = params(value)?;
            Ok(serde_json::to_value(review::submit(&p.change, p.confirm_unknown, p.confirm_outdated)?)?)
        }
        "review.markdown" => {
            let p: Markdown = params(value)?;
            Ok(Value::String(review::markdown(&p.change, &p.title)?))
        }
        "stack.list" => Ok(serde_json::to_value(stack::list(&params::<On>(value)?.change)?)?),
        "stack.whole" => Ok(serde_json::to_value(stack::whole(&params::<On>(value)?.change)?)?),
        "inbox.back_to_you" => match value.get("forge") {
            None => Ok(serde_json::to_value(turn::back_to_you())?),
            Some(forge) => {
                let forge: Forge = serde_json::from_value(forge.clone()).map_err(CommandError::Params)?;
                Ok(serde_json::to_value([turn::back_source(forge)])?)
            }
        },
        "review.since_last" => Ok(serde_json::to_value(turn::since_last(&params::<On>(value)?.change)?)?),
        "review.hand_off" => {
            let p: HandOffTo = params(value)?;
            Ok(serde_json::to_value(turn::hand_off(&p.change, &p.to)?)?)
        }
        "notify.list" => Ok(serde_json::to_value(turn::list()?)?),
        "notify.pause" => {
            let p: PauseFor = params(value)?;
            let status = if p.toggle { turn::toggle_pause()? } else { turn::pause(p.until, p.resume)? };
            Ok(serde_json::to_value(status)?)
        }
        "notify.banners" => Ok(serde_json::to_value(banner::command()?)?),
        "notify.seen" => Ok(serde_json::json!({ "marked": turn::seen(params::<Seen>(value)?.ids)? })),
        "review.blob" => {
            let p: BlobOf = params(value)?;
            Ok(view::blob_json(view::blob(&p.change, &p.path, p.side)?))
        }
        "review.set_view" => {
            let p: SetView = params(value)?;
            Ok(serde_json::to_value(view::set_view(&p.extension, p.view)?)?)
        }
        "review.views" => Ok(serde_json::to_value(view::views()?)?),
        "review.set_layout" => {
            let p: SetLayout = params(value)?;
            let host = p.host.as_deref().and_then(|h| p.forge.host_field(h));
            Ok(serde_json::to_value(review::set_layout(p.forge, host.as_deref(), &p.repo, p.options)?)?)
        }
        "review.expand" => {
            let p: Expand = params(value)?;
            Ok(serde_json::to_value(review::expand(&p.change, &p.path, p.side, p.from, p.to)?)?)
        }
        "share.render" => Ok(serde_json::to_value(share::render(&params(value)?)?)?),
        "share.save" => Err(CommandError::Invalid(
            "share.save writes the PNG with the rasteriser the engine does not link; run it through `pr-craft-cli share.save` or the app".into(),
        )),
        "share.copy" => Err(CommandError::Invalid(
            "share.copy puts the image on the clipboard of the running app; the CLI has no clipboard, use share.save to write a PNG".into(),
        )),
        "quota.status" => Ok(serde_json::to_value(quota::status(settings::load()?.poll_minutes, Utc::now()))?),
        "settings.get" => Ok(serde_json::to_value(settings::get()?)?),
        "settings.set" => Ok(serde_json::to_value(settings::set(value)?)?),
        "keys.list" => Ok(serde_json::to_value(keys::list()?)?),
        "keys.rebind" => Ok(serde_json::to_value(keys::rebind(value)?)?),
        "keys.reset" => Ok(serde_json::json!({ "reset": keys::reset(value)? })),
        "palette.query" => Ok(serde_json::to_value(palette::query(value)?)?),
        "thread.list" => Ok(serde_json::to_value(thread::list(&params::<On>(value)?.change)?)?),
        "thread.reply" => {
            let p: ReplyTo = params(value)?;
            thread::reply(&p.change, &p.thread, p.body)?;
            Ok(serde_json::json!({ "posted": true }))
        }
        "thread.resolve" => {
            let p: ResolveThread = params(value)?;
            let resolved = p.resolved.unwrap_or(true);
            thread::resolve(&p.change, &p.thread, resolved)?;
            Ok(serde_json::json!({ "resolved": resolved }))
        }
        "review.commits" => Ok(serde_json::to_value(commits::list(&params::<On>(value)?.change)?)?),
        "review.range" => {
            let p: RangeOf = params(value)?;
            Ok(serde_json::to_value(commits::range(&p.change, &p.from, &p.to)?)?)
        }
        "checkout.status" => Ok(serde_json::to_value(local::status(&params::<On>(value)?.change)?)?),
        "checkout.worktree" => Ok(serde_json::to_value(local::worktree(&params::<On>(value)?.change)?)?),
        "checkout.here" => {
            let p: CheckoutHere = params(value)?;
            Ok(serde_json::to_value(local::here(&p.change, p.confirm, p.branch.as_deref())?)?)
        }
        "open.editor" => {
            let p: OpenEditor = params(value)?;
            Ok(serde_json::to_value(local::open_editor(&p.change, p.editor, p.path.as_deref(), p.line)?)?)
        }
        id if ai::COMMANDS.contains(&id) => ai::command(&ai::Env::from_process()?, id, value),
        id if tally::COMMANDS.contains(&id) => tally::command(id, value).unwrap_or_else(|| Err(CommandError::Unknown(id.to_string()))),
        id if update::COMMANDS.contains(&id) => update::command(id, value).unwrap_or_else(|| Err(CommandError::Unknown(id.to_string()))),
        id if mine::COMMANDS.contains(&id) => mine::command(id, value).unwrap_or_else(|| Err(CommandError::Unknown(id.to_string()))),
        other => Err(CommandError::Unknown(other.to_string())),
    }
}

/// `forge.status`: the login of every forge's CLI. Blocks on the CLIs.
pub fn forge_status() -> Vec<ForgeStatus> {
    Forge::ALL.into_iter().map(|forge| ForgeStatus { forge, cli: forge.cli(), login: pr_craft_forge::login(forge) }).collect()
}

/// One forge's part of the inbox.
#[derive(Debug, Clone, Serialize)]
pub struct InboxSource {
    pub forge: Forge,
    pub cli: &'static str,
    #[serde(flatten)]
    pub state: SourceState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SourceState {
    /// At least one signed-in host answered. A host that failed is in `hosts` with its error, so the list is partial.
    Listed {
        changes: Vec<Change>,
        /// Some host held more than was read (a full GitHub page, or GitLab's page limit), so older requests may be missing.
        truncated: bool,
        /// Why stacks are unknown, when a repository's open changes could not be read in full. The changes stay ungrouped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stacks_error: Option<String>,
        /// Every signed-in host and what it answered.
        #[serde(default)]
        hosts: Vec<HostAnswer>,
        /// Hosts the CLI knows but is not signed in to. They are not read and do not make the list partial: a CLI often
        /// carries an unused gitlab.com or github.com entry.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        signed_out: Vec<String>,
    },
    /// The CLI cannot act for the user on any host; `login` carries the command that fixes it.
    Paused { login: Login },
    /// Every signed-in host failed.
    Failed { error: String },
}

/// What one signed-in host answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostAnswer {
    pub host: String,
    pub listed: usize,
    pub truncated: bool,
    /// The host could not be read; its changes are missing from the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SourceState {
    /// Whether this answer leaves changes out: the forge is paused or failed, or a signed-in host failed. An empty list is
    /// an all-clear only when this is false for every source.
    pub fn incomplete(&self) -> bool {
        match self {
            SourceState::Listed { hosts, .. } => hosts.iter().any(|h| h.error.is_some()),
            SourceState::Paused { .. } | SourceState::Failed { .. } => true,
        }
    }
}

/// `inbox.list`: every forge's open changes waiting on the user's review, both forges at once.
pub fn inbox_list() -> Vec<InboxSource> {
    std::thread::scope(|scope| {
        let running: Vec<_> = Forge::ALL.map(|forge| scope.spawn(move || inbox_source(forge))).into();
        running
            .into_iter()
            .zip(Forge::ALL)
            .map(|(job, forge)| job.join().unwrap_or_else(|_| failed(forge, "the sync thread stopped".into())))
            .collect()
    })
}

/// One forge's inbox, read from every host its CLI is signed in to, all hosts at once. Blocks on the CLI; the UI calls it
/// per forge so rows appear as each source answers.
pub fn inbox_source(forge: Forge) -> InboxSource {
    let state = match pr_craft_forge::hosts(forge) {
        Err(login) => SourceState::Paused { login },
        Ok(hosts) => {
            let (signed, out): (Vec<_>, Vec<_>) = hosts.into_iter().partition(|h| h.signed_in);
            let signed_out = out.into_iter().map(|h| h.name).collect();
            if signed.is_empty() {
                SourceState::Paused { login: pr_craft_forge::signed_out(forge) }
            } else {
                let answers: Vec<_> = std::thread::scope(|scope| {
                    let jobs: Vec<_> = signed
                        .iter()
                        .map(|h| {
                            (
                                h.name.clone(),
                                scope.spawn(move || quota::guarded(forge, &h.name, || pr_craft_forge::review_requested_on(forge, &h.name))),
                            )
                        })
                        .collect();
                    jobs.into_iter().map(|(host, job)| (host, job.join().unwrap_or_else(|_| Err("the read stopped".into())))).collect()
                });
                listed(forge, answers, signed_out)
            }
        }
    };
    InboxSource { forge, cli: forge.cli(), state }
}

/// One host's answer to `review_requested_on`, named by host.
type HostRead = (String, Result<(Vec<Change>, bool), ForgeError>);

/// Joins per-host answers: Listed while any host answered (failures stay in `hosts`), Failed when none did.
fn listed(forge: Forge, answers: Vec<HostRead>, signed_out: Vec<String>) -> SourceState {
    if answers.iter().all(|(_, a)| a.is_err()) {
        let error = answers.iter().filter_map(|(host, a)| a.as_ref().err().map(|e| format!("{host}: {e}"))).collect::<Vec<_>>().join("; ");
        return SourceState::Failed { error };
    }
    let (mut changes, mut hosts, mut truncated) = (Vec::new(), Vec::new(), false);
    for (host, answer) in answers {
        match answer {
            Ok((found, cut)) => {
                hosts.push(HostAnswer { host, listed: found.len(), truncated: cut, error: None });
                truncated |= cut;
                changes.extend(found);
            }
            Err(error) => hosts.push(HostAnswer { host, listed: 0, truncated: false, error: Some(error.message) }),
        }
    }
    changes.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    let stacks_error = stack::attach(forge, &mut changes);
    SourceState::Listed { changes, truncated, stacks_error, hosts, signed_out }
}

fn failed(forge: Forge, error: String) -> InboxSource {
    InboxSource { forge, cli: forge.cli(), state: SourceState::Failed { error } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_host_keeps_the_others_listed_but_the_list_is_not_an_all_clear() {
        // An empty answer from one host and a failure from another: nothing listed, and still not "nothing waits on you".
        let partial =
            listed(Forge::GitLab, vec![("gitlab.acme.dev".into(), Ok((Vec::new(), false))), ("gitlab.com".into(), Err("401".into()))], vec![]);
        let SourceState::Listed { hosts, changes, .. } = &partial else { panic!("one host answered, so the source is listed: {partial:?}") };
        assert!(changes.is_empty());
        assert_eq!(hosts.iter().filter(|h| h.error.is_some()).count(), 1);
        assert!(partial.incomplete(), "a host that failed leaves changes out");
        let clear = listed(Forge::GitLab, vec![("gitlab.acme.dev".into(), Ok((Vec::new(), false)))], vec!["gitlab.com".into()]);
        assert!(!clear.incomplete(), "a signed-out host the user never set up does not block the all-clear");
        let none = listed(Forge::GitLab, vec![("gitlab.com".into(), Err("401".into()))], vec![]);
        assert!(matches!(none, SourceState::Failed { ref error } if error.contains("gitlab.com")), "no host answered: {none:?}");
    }

    #[test]
    fn every_error_has_its_own_code() {
        let forge = |kind| CommandError::Forge(ForgeError::new(kind, "x"));
        let parse = serde_json::from_str::<u8>("x").expect_err("not a number");
        let table = [
            (CommandError::Unknown("x".into()), "E-UNKNOWN"),
            (CommandError::Encode(serde_json::from_str::<u8>("x").expect_err("not a number")), "E-ENCODE"),
            (CommandError::Params(parse), "E-PARAMS"),
            (CommandError::Invalid("x".into()), "E-INVALID"),
            (forge(FailKind::Auth), "E-FORGE-AUTH"),
            (forge(FailKind::Scope), "E-FORGE-SCOPE"),
            (forge(FailKind::RateLimit), "E-FORGE-RATE"),
            (forge(FailKind::Network), "E-FORGE-NET"),
            (forge(FailKind::CliMissing), "E-FORGE-CLI"),
            (forge(FailKind::CliOld), "E-FORGE-CLI-OLD"),
            (forge(FailKind::Timeout), "E-FORGE-TIMEOUT"),
            (forge(FailKind::Refused), "E-FORGE-REFUSED"),
            (forge(FailKind::Other), "E-FORGE-OTHER"),
            (CommandError::read("x"), "E-STORE-READ"),
            (CommandError::write("x"), "E-STORE-WRITE"),
        ];
        let codes: std::collections::BTreeSet<_> = table.iter().map(|(_, code)| *code).collect();
        assert_eq!(codes.len(), table.len(), "no two errors share a code");
        for (error, code) in table {
            assert_eq!(error.code(), code, "{error}");
        }
    }
}
