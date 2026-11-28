//! Back to you: the changes the user reviewed that came back, what changed since the review, handing a
//! review to someone else, and the in-app notifications built from all of it.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

use pr_craft_core::diff::{self, FileDiff};
use pr_craft_core::mine::Mine;
use pr_craft_core::turn::{Assessment, MyReview, Turn, assess, rewritten};
use pr_craft_core::{Change, ChangeRef, DateTime, Forge, Utc};
use pr_craft_forge::turn::{Candidate, Reviewed};
use pr_craft_forge::{Login, turn as forge};
use serde::Serialize;

use crate::store::{self, NotifyState, SnapshotEntry, SnapshotFile};
use crate::{CommandError, InboxSource, SourceState};

pub use crate::store::Pause;

/// What the results of `notify.*` say about banners, so no screen implies they exist.
pub const SYSTEM_NOTICE: &str = "The Dock badge, the menu-bar item and the 10:00 digest are not available yet. Banners come from Script Editor and need its notifications allowed.";

/// Serialises every load-change-save of the notification file in this process.
static NOTIFY: Mutex<()> = Mutex::new(());

/// Serialises every load-change-save of `snapshots.json` in this process.
static SNAPSHOTS: Mutex<()> = Mutex::new(());

/// A change the user reviewed, and whose turn it is.
#[derive(Debug, Clone, Serialize)]
pub struct BackItem {
    pub change: Change,
    #[serde(flatten)]
    pub assessment: Assessment,
}

/// One forge's part of Back to you.
#[derive(Debug, Clone, Serialize)]
pub struct BackSource {
    pub forge: Forge,
    pub cli: &'static str,
    #[serde(flatten)]
    pub state: BackState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackState {
    Listed {
        /// Newest update first, both back-to-you and with-the-author changes.
        items: Vec<BackItem>,
        /// The forge returned a full page, so older reviews may be missing.
        truncated: bool,
        /// Why some change is missing from `items`: its lookup failed. Never read as "with the author".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        incomplete: Option<String>,
        /// The user's own open changes where a reviewer waits on the user (changes requested, or an open thread that ends with the
        /// reviewer's comment). A failure to read them is in `incomplete`, never an empty list.
        #[serde(default)]
        yours: Vec<Mine>,
    },
    /// The CLI cannot act for the user.
    Paused {
        login: Login,
    },
    Failed {
        error: String,
    },
}

impl BackItem {
    pub fn is_back_to_you(&self) -> bool {
        matches!(self.assessment.turn, Turn::BackToYou { .. })
    }

    /// `2 new commits, 1 reply from the author`; empty while the change is with the author.
    pub fn reasons_text(&self) -> String {
        match &self.assessment.turn {
            Turn::BackToYou { reasons } => reasons.iter().map(|r| r.text()).collect::<Vec<_>>().join(", "),
            Turn::WithAuthor => String::new(),
        }
    }
}

/// `inbox.back_to_you`: every forge's reviewed changes with whose turn each is. Both forges at once.
pub fn back_to_you() -> Vec<BackSource> {
    std::thread::scope(|scope| {
        let running: Vec<_> = Forge::ALL.map(|forge| scope.spawn(move || back_source(forge))).into();
        running.into_iter().zip(Forge::ALL).map(|(job, forge)| job.join().unwrap_or_else(|_| stopped(forge))).collect()
    })
}

fn stopped(forge: Forge) -> BackSource {
    BackSource { forge, cli: forge.cli(), state: BackState::Failed { error: "the sync thread stopped".into() } }
}

/// One forge's Back to you. Blocks on its CLI; the UI calls it per forge so rows appear as each source answers.
pub fn back_source(forge: Forge) -> BackSource {
    let state = match pr_craft_forge::login(forge) {
        Login::LoggedIn => match read_source(forge) {
            Ok(state) => state,
            Err(error) => BackState::Failed { error },
        },
        login => BackState::Paused { login },
    };
    BackSource { forge, cli: forge.cli(), state }
}

/// What one forge's Back to you read found, before it is assessed.
struct Read {
    reviewed: Vec<Reviewed>,
    truncated: bool,
    /// Why the list may be short or slow to update, for the reader.
    notes: Vec<String>,
}

/// Lists the candidates, reads only those that moved since `snapshots.json` last saw them, and saves what was read.
fn read_back(forge: Forge) -> Result<Read, pr_craft_forge::ForgeError> {
    let paused = |host: &str| crate::quota::paused_until(host, Utc::now()).is_some();
    let limited = |host: &str, error: &pr_craft_forge::ForgeError| crate::quota::note_failure(forge, host, error, Utc::now());
    let listed = forge::candidates(forge, &paused, &limited)?;
    let mut notes: Vec<String> = listed
        .skipped
        .iter()
        .filter_map(|host| crate::quota::paused_until(host, Utc::now()).map(|until| crate::quota::paused_error(host, until).message))
        .map(|why| format!("{why}, so its changes are missing here."))
        .collect();
    // A baseline that cannot be read costs a full read and is left as it is; it never fails Back to you.
    let baseline = store::data_dir().and_then(|dir| store::load_snapshots(&dir).map(|file| (dir, file)));
    let (dir, stored) = match baseline {
        Ok((dir, file)) => (Some(dir), file),
        Err(error) => {
            notes.push(format!("Read every change again, since the saved reads could not be used: {error}"));
            (None, SnapshotFile::default())
        }
    };
    let reviewed = resolve(&listed.items, &stored, forge::read);
    if let Some(dir) = dir
        && let Err(error) = remember(&dir, forge, &listed, &reviewed)
    {
        notes.push(format!("Could not save what was read, so the next poll reads everything again: {error}"));
    }
    Ok(Read { reviewed, truncated: listed.truncated, notes })
}

/// One [`Reviewed`] per candidate, in order. A candidate the forge still reports at the update `stored` saw is not read
/// again; `read` gets only the others.
fn resolve(items: &[Candidate], stored: &SnapshotFile, read: impl FnOnce(&[Candidate]) -> Vec<Reviewed>) -> Vec<Reviewed> {
    let mut slots: Vec<Option<Reviewed>> = items
        .iter()
        .map(|c| stored.unmoved(&c.change, &c.me).map(|e| Reviewed { change: c.change.clone(), snapshot: Ok(e.snapshot.clone()), rate: None }))
        .collect();
    let moved: Vec<Candidate> = items.iter().zip(&slots).filter(|(_, slot)| slot.is_none()).map(|(c, _)| c.clone()).collect();
    let mut fresh = read(&moved).into_iter();
    slots.iter_mut().filter_map(|slot| slot.take().or_else(|| fresh.next())).collect()
}

/// Saves the reads of `forge` as its whole baseline. A read that failed or came back partial is not saved, so the next poll
/// asks again. The file is loaded afresh under the lock, since the other forge saves into it too.
fn remember(dir: &Path, forge: Forge, listed: &forge::Candidates, reviewed: &[Reviewed]) -> Result<(), String> {
    let fresh = listed
        .items
        .iter()
        .zip(reviewed)
        .filter_map(|(c, r)| match &r.snapshot {
            Ok(snapshot) if snapshot.as_ref().is_none_or(|s| s.incomplete.is_none()) => {
                let entry =
                    SnapshotEntry { updated_at: r.change.updated_at, me: c.me.clone(), snapshot: snapshot.clone(), extra: Default::default() };
                Some((store::snapshot_key(&r.change.change_ref()), entry))
            }
            _ => None,
        })
        .collect();
    let _held = SNAPSHOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut file = store::load_snapshots(dir)?;
    file.replace(forge, &listed.skipped, fresh);
    store::save_snapshots(dir, &file)
}

fn read_source(forge: Forge) -> Result<BackState, String> {
    let (list, (yours, yours_note)) = std::thread::scope(|scope| {
        let mine = scope.spawn(move || crate::mine::waiting_on_you(forge));
        (read_back(forge), mine.join().unwrap_or_else(|_| (Vec::new(), Some("The read of your own changes stopped.".to_string()))))
    });
    let list = list.map_err(|e| e.message)?;
    let (mut items, failed, partial) = assess_all(forge, list.reviewed);
    items.sort_by_key(|item| std::cmp::Reverse(item.change.updated_at));
    let mut notes = Vec::new();
    if !failed.is_empty() {
        notes.push(format!("Could not read {}, so they are missing here.", failed.join("; ")));
    }
    if !partial.is_empty() {
        notes.push(format!("Read only part of {}, so whose turn it is may be wrong.", partial.join("; ")));
    }
    notes.extend(list.notes);
    notes.extend(yours_note);
    let incomplete = (!notes.is_empty()).then(|| notes.join(" "));
    Ok(BackState::Listed { items, truncated: list.truncated, incomplete, yours })
}

/// The rows of what was read, the changes whose lookup failed, and those read only in part.
fn assess_all(forge: Forge, reviewed: Vec<Reviewed>) -> (Vec<BackItem>, Vec<String>, Vec<String>) {
    let (mut items, mut failed, mut partial) = (Vec::new(), Vec::new(), Vec::new());
    for reviewed in reviewed {
        if let Some(rate) = reviewed.rate {
            crate::quota::record(reviewed.change.host.as_deref().unwrap_or(forge.default_host()), rate);
        }
        match reviewed.snapshot {
            Ok(Some(snapshot)) => {
                if let Some(why) = &snapshot.incomplete {
                    partial.push(format!("{} ({why})", reviewed.change.reference()));
                }
                items.push(BackItem { change: reviewed.change, assessment: assess(&snapshot) });
            }
            Ok(None) => {}
            Err(error) => failed.push(format!("{} ({error})", reviewed.change.reference())),
        }
    }
    (items, failed, partial)
}

/// `review.since_last`: what changed after the user's last review of a change.
#[derive(Debug, Clone, Serialize)]
pub struct SinceLast {
    pub change: ChangeRef,
    pub review: MyReview,
    /// The commit the review was written on.
    pub from: String,
    /// The change's head commit now.
    pub to: String,
    pub commits_since: u32,
    pub files: Vec<FileDiff>,
    /// Paths in `files` the forge sent without lines because they are too large; they show no hunks.
    pub omitted: Vec<String>,
    /// The commit the review was written on is no longer part of the change (force-push or rebase), so `files` is the
    /// whole change as it stands, not only what is new.
    pub rewritten: bool,
    pub truncated: bool,
    pub web_url: Option<String>,
}

/// `review.since_last`: the files changed between the commit of the user's last review and the head now. Blocks on the CLI.
pub fn since_last(change: &ChangeRef) -> Result<SinceLast, CommandError> {
    let me = forge::my_login(change.forge, change.host_name()).map_err(CommandError::Forge)?;
    let snapshot = forge::snapshot(change, &me)
        .map_err(CommandError::Forge)?
        .0
        .ok_or_else(|| CommandError::Invalid(format!("You have not reviewed {}, so there is no review to compare with.", change.reference())))?;
    let Assessment { review, commits_since, .. } = assess(&snapshot);
    let from = review.commit.clone().ok_or_else(|| {
        CommandError::Invalid(format!("{} holds no commit your review was written on, so what changed since cannot be found.", change.forge.name()))
    })?;
    let gone = rewritten(&snapshot);
    let to = snapshot.head;
    let (files, omitted, web_url) = if gone {
        // The reviewed commit may no longer exist in the repository, so there is nothing to compare from: show the whole change.
        let text = pr_craft_forge::review::diff(change).map_err(CommandError::Forge)?;
        (diff::parse_unified(&text), Vec::new(), None)
    } else if from == to {
        (Vec::new(), Vec::new(), None)
    } else {
        let compare = forge::compare_commits(change, &from, &to).map_err(CommandError::Forge)?;
        (diff::parse_unified(&compare.text), compare.omitted, compare.web_url)
    };
    Ok(SinceLast {
        change: change.clone(),
        review,
        from,
        to,
        commits_since,
        truncated: !omitted.is_empty(),
        files,
        omitted,
        rewritten: gone,
        web_url,
    })
}

/// `review.hand_off`: the result of handing a review to someone.
#[derive(Debug, Clone, Serialize)]
pub struct HandOff {
    pub change: ChangeRef,
    pub from: String,
    pub to: String,
    /// What happened, for the screen to show.
    pub message: String,
}

/// A login to hand off to, without a leading `@`. Refuses an empty one, and anything that is not a login, since
/// GitLab looks it up in a URL.
fn login_of(to: &str) -> Result<&str, CommandError> {
    let to = to.trim().trim_start_matches('@');
    if to.is_empty() {
        return Err(CommandError::Invalid("Say who takes over this review: a login is needed.".into()));
    }
    if !to.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(CommandError::Invalid(format!("\"{to}\" is not a login. Use letters, digits, - _ and . only.")));
    }
    Ok(to)
}

/// `review.hand_off`: asks `to` to review and takes the user off the change. This writes to the forge.
pub fn hand_off(change: &ChangeRef, to: &str) -> Result<HandOff, CommandError> {
    let to = login_of(to)?;
    let me = forge::my_login(change.forge, change.host_name()).map_err(CommandError::Forge)?;
    if to.eq_ignore_ascii_case(&me) {
        return Err(CommandError::Invalid("That login is yours. Name someone else.".into()));
    }
    forge::hand_off(change, &me, to).map_err(CommandError::Forge)?;
    let message = format!(
        "Handed off to {to}. {to} was asked to review {} and you are no longer a reviewer on it. Your comments stay on the change.",
        change.reference()
    );
    Ok(HandOff { change: change.clone(), from: me, to: to.to_string(), message })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    BackToYou,
    ReviewRequested,
}

/// One in-app notification.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// Stable while nothing new happens, and new again when more does (more commits, a new review of the user's).
    pub id: String,
    pub kind: EventKind,
    pub change: Change,
    pub text: String,
    pub at: DateTime<Utc>,
    pub seen: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Notifications {
    /// Newest first.
    pub events: Vec<Event>,
    pub unseen: usize,
    /// Sources that could not be read, so their events are missing rather than absent.
    pub errors: Vec<String>,
    /// Set while notifications are paused.
    pub pause: Option<Pause>,
    pub system_notice: &'static str,
}

/// What an own change a reviewer waits on says, and what keys it: who asked for changes, or how many threads wait. The status line
/// is not used, because CI flipping would make the same request look new.
fn yours_words(mine: &Mine) -> (String, String) {
    let plural = |n: u32| if n == 1 { "1 thread".to_string() } else { format!("{n} threads") };
    if mine.decision == pr_craft_core::mine::Decision::ChangesRequested {
        let who = mine.changes_requested.first().map(|c| c.by.as_str());
        let open = if mine.threads_open > 0 { format!(" · {} open", plural(mine.threads_open)) } else { String::new() };
        return (format!("changes:{}", who.unwrap_or("")), format!("{} requested changes{open}", who.unwrap_or("A reviewer")));
    }
    (format!("threads:{}", mine.threads_waiting), format!("{} wait on you", plural(mine.threads_waiting)))
}

/// The events of `back` and `requested`, newest first, with the problems that make some of them unknown.
/// A review request is an event while it is open; a change back to the user is one per set of reasons.
pub fn events(back: &[BackSource], requested: &[InboxSource], seen: &BTreeSet<String>) -> (Vec<Event>, Vec<String>) {
    let mut events = Vec::new();
    let mut problems = BTreeSet::new();
    let paused = |forge: Forge| format!("{}: {} cannot act for you yet, so its notifications are missing.", forge.name(), forge.cli());
    for source in back {
        match &source.state {
            BackState::Listed { items, truncated, incomplete, yours } => {
                for mine in yours {
                    let (key, text) = yours_words(mine);
                    events.push(event(format!("yours:{}:{key}", mine.change.reference()), EventKind::BackToYou, &mine.change, text, seen));
                }
                for item in items {
                    let Turn::BackToYou { reasons } = &item.assessment.turn else { continue };
                    let keys: Vec<String> = reasons.iter().map(|r| r.key()).collect();
                    let stamp = item.assessment.review.submitted_at.map_or(0, |at| at.timestamp());
                    let id = format!("back:{}:{stamp}:{}", item.change.reference(), keys.join("+"));
                    let text = format!("{}: {}", item.change.author, item.reasons_text());
                    events.push(event(id, EventKind::BackToYou, &item.change, text, seen));
                }
                if let Some(incomplete) = incomplete {
                    problems.insert(format!("{} reviews: {incomplete}", source.forge.name()));
                }
                if *truncated {
                    problems.insert(limit_problem(source.forge, "reviews"));
                }
            }
            BackState::Paused { .. } => {
                problems.insert(paused(source.forge));
            }
            BackState::Failed { error } => {
                problems.insert(format!("{}: could not read the changes you reviewed: {error}", source.forge.name()));
            }
        }
    }
    for source in requested {
        match &source.state {
            SourceState::Listed { changes, truncated, .. } => {
                if *truncated {
                    problems.insert(limit_problem(source.forge, "review requests"));
                }
                for change in changes {
                    let id = format!("requested:{}", change.reference());
                    events.push(event(id, EventKind::ReviewRequested, change, format!("{} asked you to review", change.author), seen));
                }
            }
            SourceState::Paused { .. } => {
                problems.insert(paused(source.forge));
            }
            SourceState::Failed { error } => {
                problems.insert(format!("{}: could not read your review requests: {error}", source.forge.name()));
            }
        }
    }
    events.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
    (events, problems.into_iter().collect())
}

/// Rule 15: a list cut at the page limit is not the whole list.
fn limit_problem(forge: Forge, what: &str) -> String {
    format!("{}: {} returned its limit of {} {what}, so older ones may be missing.", forge.name(), forge.cli(), pr_craft_forge::PAGE)
}

fn event(id: String, kind: EventKind, change: &Change, text: String, seen: &BTreeSet<String>) -> Event {
    Event { seen: seen.contains(&id), id, kind, change: change.clone(), text, at: change.updated_at }
}

/// The pause that still holds at `now`: open-ended, or until a time not yet reached.
fn active_pause(pause: Option<Pause>, now: DateTime<Utc>) -> Option<Pause> {
    pause.filter(|p| p.until.is_none_or(|until| until > now))
}

/// The notifications for sources already read, with seen marks and the pause from this Mac. Reads no forge.
pub fn compose(back: &[BackSource], requested: &[InboxSource]) -> Result<Notifications, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let state = {
        let _guard = lock()?;
        store::load_notify(&dir).map_err(CommandError::read)?
    };
    let (events, errors) = events(back, requested, &state.seen);
    let unseen = events.iter().filter(|e| !e.seen).count();
    Ok(Notifications { events, unseen, errors, pause: active_pause(state.pause, Utc::now()), system_notice: SYSTEM_NOTICE })
}

/// `notify.list`: reads both forges, then [`compose`]s. Blocks on the CLIs.
pub fn list() -> Result<Notifications, CommandError> {
    let (back, requested) = std::thread::scope(|scope| {
        let back = scope.spawn(back_to_you);
        let requested = crate::inbox_list();
        (back.join().unwrap_or_else(|_| Forge::ALL.map(stopped).into()), requested)
    });
    compose(&back, &requested)
}

/// `notify.pause`: where notifications stand after the change.
#[derive(Debug, Clone, Serialize)]
pub struct PauseStatus {
    /// Set while notifications are paused.
    pub pause: Option<Pause>,
    pub system_notice: &'static str,
}

/// `notify.pause`: pauses notifications until `until`, or until resumed when `until` is `None`; `resume` ends a pause.
/// `notify.banners` posts nothing while a pause holds.
pub fn pause(until: Option<DateTime<Utc>>, resume: bool) -> Result<PauseStatus, CommandError> {
    if until.is_some_and(|until| until <= Utc::now()) {
        return Err(CommandError::Invalid("`until` is already past.".into()));
    }
    set_pause(|_| (!resume).then_some(Pause { until, extra: Default::default() }))
}

/// `notify.pause` with `toggle`: ends the pause that holds now, or starts one until resumed. It reads the stored
/// state, so a screen showing an older state never flips the wrong way.
pub fn toggle_pause() -> Result<PauseStatus, CommandError> {
    set_pause(|now| if now.is_some() { None } else { Some(Pause { until: None, extra: Default::default() }) })
}

/// Loads the stored pause, lets `change` replace it (it sees the pause that holds now), and saves it.
fn set_pause(change: impl FnOnce(Option<Pause>) -> Option<Pause>) -> Result<PauseStatus, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let _guard = lock()?;
    let mut state = store::load_notify(&dir).map_err(CommandError::read)?;
    state.pause = change(active_pause(state.pause.take(), Utc::now()));
    store::save_notify(&dir, &state).map_err(CommandError::write)?;
    Ok(PauseStatus { pause: state.pause, system_notice: SYSTEM_NOTICE })
}

/// `notify.seen`: marks events seen. Returns how many were not seen before.
pub fn seen(ids: Vec<String>) -> Result<usize, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let _guard = lock()?;
    let mut state: NotifyState = store::load_notify(&dir).map_err(CommandError::read)?;
    let now = Utc::now();
    let added = ids
        .into_iter()
        .filter(|id| {
            state.seen_at.entry(id.clone()).or_insert(now);
            state.seen.insert(id.clone())
        })
        .count();
    store::save_notify(&dir, &state).map_err(CommandError::write)?;
    Ok(added)
}

pub(crate) fn lock() -> Result<std::sync::MutexGuard<'static, ()>, CommandError> {
    NOTIFY.lock().map_err(|_| CommandError::write("the notification state was left half-written by a crash in this process"))
}

#[cfg(test)]
mod tests {
    use pr_craft_core::turn::{MyReview, Reason, ReviewState};

    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-11-{day:02}T09:00:00Z")).expect("a valid time").with_timezone(&Utc)
    }

    // An invented change shaped like `gh search prs` output.
    fn change(number: u64, updated: u32) -> Change {
        Change {
            forge: Forge::GitHub,
            repo: "acme/api".into(),
            number,
            title: format!("Change {number}"),
            author: "nina".into(),
            url: format!("https://github.com/acme/api/pull/{number}"),
            updated_at: at(updated),
            draft: false,
            host: None,
            stack: None,
        }
    }

    fn item(number: u64, updated: u32, turn: Turn) -> BackItem {
        let review = MyReview { state: ReviewState::ChangesRequested, submitted_at: Some(at(10)), commit: Some("a1".into()) };
        BackItem { change: change(number, updated), assessment: Assessment { review, commits_since: 0, turn } }
    }

    fn back(items: Vec<BackItem>) -> BackSource {
        BackSource { forge: Forge::GitHub, cli: "gh", state: BackState::Listed { items, truncated: false, incomplete: None, yours: Vec::new() } }
    }

    fn requested(changes: Vec<Change>) -> InboxSource {
        InboxSource {
            forge: Forge::GitHub,
            cli: "gh",
            state: SourceState::Listed { changes, truncated: false, stacks_error: None, hosts: Vec::new(), signed_out: Vec::new() },
        }
    }

    #[test]
    fn an_empty_or_malformed_login_is_refused_before_anything_is_sent() {
        assert!(login_of("").is_err());
        assert!(login_of("  @ ").is_err());
        assert!(login_of("j park").is_err());
        assert!(login_of("a&reviewer_ids=1").is_err(), "GitLab reads it inside a URL");
        assert_eq!(login_of(" @j.park ").ok(), Some("j.park"));
    }

    #[test]
    fn events_come_from_reasons_and_review_requests_newest_first_and_only_for_my_turn() {
        let reasons = vec![Reason::NewCommits { count: 2 }, Reason::AuthorReplied { count: 1, last_reply: at(11) }];
        let sources = [back(vec![item(1, 12, Turn::BackToYou { reasons }), item(2, 18, Turn::WithAuthor)])];
        let (events, problems) = events(&sources, &[requested(vec![change(3, 15)])], &BTreeSet::new());
        assert!(problems.is_empty());
        let shown: Vec<(&str, EventKind)> = events.iter().map(|e| (e.text.as_str(), e.kind)).collect();
        assert_eq!(
            shown,
            [("nina asked you to review", EventKind::ReviewRequested), ("nina: 2 new commits, 1 reply from the author", EventKind::BackToYou)]
        );
    }

    #[test]
    fn an_own_change_a_reviewer_waits_on_is_an_event_with_the_reviewers_words() {
        let mut mine = crate::mine::tests_support::mine(7);
        mine.reviewers = vec!["nina.k".into(), "j.park".into()];
        mine.decision = pr_craft_core::mine::Decision::ChangesRequested;
        mine.changes_requested = vec![pr_craft_core::mine::ChangesRequest { by: "j.park".into(), at: None }];
        mine.threads_open = 2;
        let id = |m: &Mine| {
            let state = BackState::Listed { items: Vec::new(), truncated: false, incomplete: None, yours: vec![m.clone()] };
            let (events, problems) = events(&[BackSource { forge: Forge::GitHub, cli: "gh", state }], &[], &BTreeSet::new());
            assert!(problems.is_empty());
            (events[0].id.clone(), events[0].text.clone())
        };
        let (first, text) = id(&mine);
        assert_eq!(text, "j.park requested changes · 2 threads open", "the one who asked, not the first reviewer listed");
        mine.checks.state = Some(pr_craft_core::mine::CheckState::Failing);
        assert_eq!(id(&mine).0, first, "CI going red does not make the same request new again");
    }

    #[test]
    fn more_commits_make_the_event_unseen_again() {
        let turn = |count| Turn::BackToYou { reasons: vec![Reason::NewCommits { count }] };
        let (first, _) = events(&[back(vec![item(1, 12, turn(1))])], &[], &BTreeSet::new());
        let seen: BTreeSet<String> = first.iter().map(|e| e.id.clone()).collect();
        let (same, _) = events(&[back(vec![item(1, 12, turn(1))])], &[], &seen);
        assert!(same.iter().all(|e| e.seen));
        let (more, _) = events(&[back(vec![item(1, 13, turn(3))])], &[], &seen);
        assert!(more.iter().all(|e| !e.seen), "three commits are news after one was seen");
    }

    #[test]
    fn a_source_that_failed_or_is_paused_is_a_problem_not_silence() {
        let failed = BackSource { forge: Forge::GitLab, cli: "glab", state: BackState::Failed { error: "timeout".into() } };
        let paused = InboxSource {
            forge: Forge::GitLab,
            cli: "glab",
            state: SourceState::Paused { login: Login::LoggedOut { fix: "glab auth login".into() } },
        };
        let (events, problems) = events(&[failed], &[paused], &BTreeSet::new());
        assert!(events.is_empty());
        assert_eq!(problems.len(), 2);
        assert!(problems.iter().any(|p| p.contains("timeout")));
    }

    #[test]
    fn a_list_cut_at_the_page_limit_is_a_problem_not_a_short_list() {
        let cut = BackSource {
            forge: Forge::GitHub,
            cli: "gh",
            state: BackState::Listed { items: Vec::new(), truncated: true, incomplete: None, yours: Vec::new() },
        };
        let requests = InboxSource {
            forge: Forge::GitHub,
            cli: "gh",
            state: SourceState::Listed { changes: Vec::new(), truncated: true, stacks_error: None, hosts: Vec::new(), signed_out: Vec::new() },
        };
        let (events, problems) = events(&[cut], &[requests], &BTreeSet::new());
        assert!(events.is_empty());
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().all(|p| p.contains("older ones may be missing")));
    }
    #[test]
    fn a_pause_holds_until_its_time_and_open_ended_holds_until_resumed() {
        let now = at(20);
        assert!(active_pause(Some(Pause { until: None, extra: Default::default() }), now).is_some());
        assert!(active_pause(Some(Pause { until: Some(at(21)), extra: Default::default() }), now).is_some());
        assert!(active_pause(Some(Pause { until: Some(at(19)), extra: Default::default() }), now).is_none());
        assert!(active_pause(None, now).is_none());
    }

    fn candidate(number: u64, updated: u32) -> Candidate {
        Candidate { change: change(number, updated), me: "ana".into() }
    }

    /// What a forge would say of `c`: reviewed on the 10th, one commit at the change's update, so back to you after the 10th.
    fn snapshot_of(c: &Candidate) -> pr_craft_core::turn::Snapshot {
        let commit = |sha: &str, at| pr_craft_core::turn::Commit { sha: sha.into(), at };
        pr_craft_core::turn::Snapshot {
            me: c.me.clone(),
            author: "nina".into(),
            review: MyReview { state: ReviewState::Commented, submitted_at: Some(at(10)), commit: Some("a1".into()) },
            head: format!("n{}", c.change.number),
            commits: vec![commit("a1", at(9)), commit(&format!("n{}", c.change.number), c.change.updated_at)],
            threads: Vec::new(),
            notes: Vec::new(),
            re_requested: false,
            re_requested_at: None,
            approval_dismissed: false,
            review_id: None,
            commits_complete: true,
            incomplete: None,
        }
    }

    fn full_read(items: &[Candidate]) -> Vec<Reviewed> {
        items.iter().map(|c| Reviewed { change: c.change.clone(), snapshot: Ok(Some(snapshot_of(c))), rate: None }).collect()
    }

    fn listed(items: Vec<Candidate>) -> forge::Candidates {
        forge::Candidates { items, truncated: false, skipped: Vec::new() }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pr-craft-turn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn rows(reviewed: Vec<Reviewed>) -> serde_json::Value {
        let (items, failed, partial) = assess_all(Forge::GitHub, reviewed);
        assert!((failed.is_empty(), partial.is_empty()) == (true, true));
        serde_json::to_value(items).expect("rows")
    }

    #[test]
    fn a_persisted_baseline_reads_only_what_moved_and_the_rows_equal_a_full_read() {
        let dir = temp_dir("baseline");
        let first = vec![candidate(1, 12), candidate(2, 11), candidate(3, 5)];
        let reviewed = resolve(&first, &SnapshotFile::default(), full_read);
        remember(&dir, Forge::GitHub, &listed(first), &reviewed).expect("saved");

        // The next poll, as a new process: #2 moved, #1 and #3 did not.
        let second = vec![candidate(1, 12), candidate(2, 14), candidate(3, 5)];
        let stored = store::load_snapshots(&dir).expect("loaded");
        let read = std::cell::RefCell::new(Vec::new());
        let resolved = resolve(&second, &stored, |moved| {
            read.borrow_mut().extend(moved.iter().map(|c| c.change.number));
            full_read(moved)
        });
        assert_eq!(*read.borrow(), [2], "only the moved change was read");
        let (resolved, full) = (rows(resolved), rows(full_read(&second)));
        assert_eq!(resolved, full, "the same assessment and rows as a full read");
        let items = resolved.as_array().expect("rows");
        assert_eq!(
            items.iter().filter(|i| i["turn"] == "back_to_you").count(),
            3,
            "unmoved changes stay in Back to you with their reasons: {items:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_change_that_is_no_longer_listed_is_dropped_from_the_saved_file() {
        let dir = temp_dir("dropped");
        let items = vec![candidate(1, 12), candidate(2, 11)];
        remember(&dir, Forge::GitHub, &listed(items.clone()), &full_read(&items)).expect("saved");
        let kept = vec![items[0].clone()];
        remember(&dir, Forge::GitHub, &listed(kept.clone()), &full_read(&kept)).expect("saved");
        let keys: Vec<_> = store::load_snapshots(&dir).expect("loaded").entries.into_keys().collect();
        assert_eq!(keys, ["gh|github.com|acme/api|1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
