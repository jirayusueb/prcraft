//! Back to you: the changes the user reviewed that came back, what changed since the review, handing a
//! review to someone else, and the in-app notifications built from all of it.

use std::collections::BTreeSet;
use std::sync::Mutex;

use pr_craft_core::diff::{self, FileDiff};
use pr_craft_core::mine::Mine;
use pr_craft_core::turn::{Assessment, MyReview, Turn, assess, rewritten};
use pr_craft_core::{Change, ChangeRef, DateTime, Forge, Utc};
use pr_craft_forge::{Login, turn as forge};
use serde::Serialize;

use crate::store::{self, NotifyState};
use crate::{CommandError, InboxSource, SourceState};

pub use crate::store::Pause;

/// What the results of `notify.*` say about banners, so no screen implies they exist.
pub const SYSTEM_NOTICE: &str =
    "System banners, the Dock badge, the menu-bar item and the 10:00 digest are not available yet. Notifications show in PR Craft only.";

/// Serialises every load-change-save of the notification file in this process.
static NOTIFY: Mutex<()> = Mutex::new(());

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

fn read_source(forge: Forge) -> Result<BackState, String> {
    let (list, (yours, yours_note)) = std::thread::scope(|scope| {
        let mine = scope.spawn(move || crate::mine::waiting_on_you(forge));
        (forge::reviewed(forge), mine.join().unwrap_or_else(|_| (Vec::new(), Some("The read of your own changes stopped.".to_string()))))
    });
    let list = list?;
    let (mut items, mut failed, mut partial) = (Vec::new(), Vec::new(), Vec::new());
    for reviewed in list.items {
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
    items.sort_by_key(|item| std::cmp::Reverse(item.change.updated_at));
    let mut notes = Vec::new();
    if !failed.is_empty() {
        notes.push(format!("Could not read {}, so they are missing here.", failed.join("; ")));
    }
    if !partial.is_empty() {
        notes.push(format!("Read only part of {}, so whose turn it is may be wrong.", partial.join("; ")));
    }
    notes.extend(yours_note);
    let incomplete = (!notes.is_empty()).then(|| notes.join(" "));
    Ok(BackState::Listed { items, truncated: list.truncated, incomplete, yours })
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
    let dir = store::data_dir().map_err(CommandError::Store)?;
    let state = {
        let _guard = lock()?;
        store::load_notify(&dir).map_err(CommandError::Store)?
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
/// Nothing shows banners yet, so a pause is only kept and shown.
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
    let dir = store::data_dir().map_err(CommandError::Store)?;
    let _guard = lock()?;
    let mut state = store::load_notify(&dir).map_err(CommandError::Store)?;
    state.pause = change(active_pause(state.pause.take(), Utc::now()));
    store::save_notify(&dir, &state).map_err(CommandError::Store)?;
    Ok(PauseStatus { pause: state.pause, system_notice: SYSTEM_NOTICE })
}

/// `notify.seen`: marks events seen. Returns how many were not seen before.
pub fn seen(ids: Vec<String>) -> Result<usize, CommandError> {
    let dir = store::data_dir().map_err(CommandError::Store)?;
    let _guard = lock()?;
    let mut state: NotifyState = store::load_notify(&dir).map_err(CommandError::Store)?;
    let added = ids.into_iter().filter(|id| state.seen.insert(id.clone())).count();
    store::save_notify(&dir, &state).map_err(CommandError::Store)?;
    Ok(added)
}

fn lock() -> Result<std::sync::MutexGuard<'static, ()>, CommandError> {
    NOTIFY.lock().map_err(|_| CommandError::Store("the notification state was left half-written by a crash in this process".into()))
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
}
