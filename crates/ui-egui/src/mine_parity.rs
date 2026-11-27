//! Your changes drawn offscreen at the size of Figma frames 27, 75, 97, 94, 88 and 89, with invented data that mirrors the frames'
//! sample, and the tests that read what they say. Test-only: a fixture app reads no forge.

use chrono::{Duration, Utc};
use craft::egui::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use pr_craft_core::mine::{
    ChangesRequest, Check, CheckState, Checks, Decision, JobLog, LinkedIssue, MergeMethod, MergeQueue, MergeState, Mine, MineDetail, parse_log,
};
use pr_craft_core::review::LineRef;
use pr_craft_core::thread::{Original, ReviewThread, ThreadComment};
use pr_craft_core::{Change, ChangeRef, Forge};
use pr_craft_engine::mine::{ChecksList, LogResult};
use pr_craft_engine::thread::ThreadList;
use pr_craft_forge::act::{Label, Person};

use crate::checks::ChecksScreen;
use crate::mine::MineSync;
use crate::panel::Panel;
use crate::parity::{harness, inbox_app, named, says};
use crate::{Inbox, PrCraftApp};

fn base(forge: Forge, repo: &str, number: u64, title: &str, size: (u64, u64), hours: i64) -> Mine {
    let host = if forge == Forge::GitHub { "github.com" } else { "gitlab.acme.dev" };
    Mine {
        change: Change {
            forge,
            repo: repo.into(),
            number,
            title: title.into(),
            author: "maria".into(),
            url: format!("https://{host}/{repo}/{number}"),
            host: None,
            updated_at: Utc::now() - Duration::hours(hours),
            draft: false,
            stack: None,
        },
        additions: Some(size.0),
        deletions: Some(size.1),
        head_branch: "maria/jitter".into(),
        head_sha: "7c2e1f0".into(),
        base_branch: "main".into(),
        commits: Some(2),
        reviewers: vec!["nina.k".into(), "j.park".into()],
        requested: vec!["nina.k".into(), "j.park".into()],
        decision: Decision::None,
        approvals: 0,
        approvals_required: None,
        changes_requested: Vec::new(),
        threads_open: 0,
        threads_waiting: 0,
        checks: passing(14),
        merge: MergeState::Ready,
        merge_note: None,
        auto_merge: false,
        labels: Vec::new(),
        unread: Vec::new(),
    }
}

fn passing(n: u32) -> Checks {
    Checks { state: Some(CheckState::Passing), total: n, passed: n, failed: 0, running: 0, failed_names: Vec::new() }
}

/// The first row of the frames: acme/api #490.
fn first() -> Mine {
    base(Forge::GitHub, "acme/api", 490, "Use crypto/rand for retry jitter", (9, 3), 2)
}

fn others() -> (Mine, Mine) {
    let mut lint = base(Forge::GitLab, "billing-service", 1288, "Fix lint in invoice exporter", (3, 2), 24);
    lint.reviewers = vec!["t.okafor".into()];
    lint.requested = lint.reviewers.clone();
    let mut finalize = base(Forge::GitHub, "acme/web", 2216, "Drop the legacy finalize path", (12, 140), 48);
    (finalize.decision, finalize.auto_merge, finalize.approvals) = (Decision::Approved, true, 2);
    (lint, finalize)
}

fn failing(mut m: Mine) -> Mine {
    m.checks = Checks { state: Some(CheckState::Failing), total: 14, passed: 13, failed: 1, running: 0, failed_names: vec!["test:unit".into()] };
    m.merge = MergeState::Blocked;
    m.merge_note = Some("A required review or check is missing".into());
    m
}

fn requested_changes(mut m: Mine) -> Mine {
    m.decision = Decision::ChangesRequested;
    m.threads_open = 2;
    m.threads_waiting = 2;
    m.changes_requested = vec![ChangesRequest { by: "j.park".into(), at: Some(Utc::now() - Duration::hours(3)) }];
    m.merge = MergeState::Blocked;
    failing(m)
}

fn draft(mut m: Mine) -> Mine {
    m.change.draft = true;
    m.reviewers.clear();
    m.requested.clear();
    m.labels = vec!["bug".into(), "webhooks".into(), "flaky-test".into()];
    m
}

fn ready(mut m: Mine) -> Mine {
    (m.decision, m.approvals, m.approvals_required) = (Decision::Approved, 2, Some(2));
    m
}

fn detail(mine: Mine) -> MineDetail {
    let message = format!(
        "fix(retry): give each worker its own jitter source (#{})\n\nWorkers shared one math/rand source, so retries bunched at the same second. Each worker now seeds its own. Fixes test:unit flake.",
        mine.change.number
    );
    let checks = if mine.checks.state == Some(CheckState::Failing) { check_list().checks } else { Vec::new() };
    MineDetail {
        mine,
        checks,
        checks_error: None,
        queue: None,
        closes: Vec::new(),
        methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
        message,
        queue_error: None,
    }
}

fn app(first: Mine, detail: MineDetail, threads: Option<ThreadList>, picks: bool) -> PrCraftApp {
    let (lint, finalize) = others();
    let mut app = inbox_app();
    app.view = Inbox::Mine;
    app.selected = Some(first.change.url.clone());
    let people = if picks { people() } else { Vec::new() };
    let labels = if picks { labels() } else { Vec::new() };
    app.panel = Some(Panel::fixture(&first, detail, threads, people, labels));
    app.mine = vec![MineSync::fixture(Forge::GitHub, vec![first, finalize]), MineSync::fixture(Forge::GitLab, vec![lint])];
    app
}

fn people() -> Vec<Person> {
    [("j.park", "write access"), ("nina.k", "write access"), ("dev.sato", "maintain access"), ("maria", "admin access")]
        .into_iter()
        .map(|(login, role)| Person { login: login.into(), name: None, role: role.into() })
        .collect()
}

fn labels() -> Vec<Label> {
    ["bug", "webhooks", "flaky-test", "docs"].into_iter().map(|name| Label { name: name.into(), color: None, description: String::new() }).collect()
}

fn threads() -> ThreadList {
    let thread = |id: &str, path: &str, line: u32, resolved: bool, notes: &[(&str, &str)]| ReviewThread {
        id: id.into(),
        path: path.into(),
        line: Some(LineRef { old: None, new: Some(line) }),
        original: Original::default(),
        resolved,
        resolved_by: resolved.then(|| "j.park".to_string()),
        outdated: false,
        can_resolve: true,
        can_reply: true,
        comments: notes.iter().map(|(a, b)| ThreadComment { author: (*a).into(), at: None, body: (*b).into() }).collect(),
        comments_total: 0,
    };
    ThreadList {
        change: first().change.change_ref(),
        threads: vec![
            thread("a", "retry.go", 41, false, &[("j.park", "The jitter source is shared across workers; seed one per worker.")]),
            thread("b", "retry_test.go", 20, false, &[("j.park", "This test passes by luck; assert the delay range instead.")]),
            thread("c", "worker.go", 12, true, &[("j.park", "Nit: name it maxAttempts."), ("maria", "Renamed in a41c9e.")]),
        ],
        complete: true,
        note: None,
    }
}

fn check_list() -> ChecksList {
    let check = |id: &str, name: &str, state: CheckState, seconds: u64| Check {
        id: id.into(),
        name: name.into(),
        group: None,
        state,
        seconds: Some(seconds),
        url: Some(format!("https://github.com/acme/api/actions/runs/48213/job/{id}")),
        has_log: true,
    };
    let checks = vec![
        check("1", "build", CheckState::Passing, 72),
        check("2", "lint", CheckState::Passing, 24),
        check("3", "test:unit", CheckState::Failing, 41),
        check("4", "test:integration", CheckState::Passing, 182),
        check("5", "security/codeql", CheckState::Passing, 160),
    ];
    ChecksList { change: first().change.change_ref(), summary: Checks::of(&checks), checks }
}

fn log() -> LogResult {
    let lines = [
        "Set up Go 1.22.4",
        "go mod download",
        "=== RUN   TestRetry_Jitter",
        "=== PAUSE TestRetry_Jitter",
        "=== CONT  TestRetry_Jitter",
        "    retry_test.go:24: delay out of range: 1.94s (want 2s-2.4s)",
        "    retry_test.go:24: delay out of range: 1.97s (want 2s-2.4s)",
        "--- FAIL: TestRetry_Jitter (0.03s)",
        "=== RUN   TestRetry_GivesUp",
        "--- done: TestRetry_GivesUp (0.00s)",
        "FAIL\tgithub.com/acme/api/internal/webhooks\t0.412s",
        "ok  \tgithub.com/acme/api/internal/config\t0.108s",
        "Error: Process completed with exit code 1.",
    ];
    let log: JobLog = parse_log(&lines.join("\n"), false);
    LogResult { change: first().change.change_ref(), check: "3".into(), log }
}

/// Frame 27: the failing checks banner over Your changes, with the Babysit card as a note that it is not built.
pub(crate) fn failing_app() -> PrCraftApp {
    let m = failing(first());
    app(m.clone(), detail(m), None, false)
}

/// Frame 75: a reviewer asked for changes; the threads wait.
pub(crate) fn author_app() -> PrCraftApp {
    let m = requested_changes(first());
    app(m.clone(), detail(m), Some(threads()), false)
}

/// Frame 97: a draft with reviewers and labels to pick.
pub(crate) fn draft_app() -> PrCraftApp {
    let m = draft(first());
    let mut d = detail(m.clone());
    d.closes = vec![LinkedIssue { number: 471, title: "Retries bunch at the same second".into() }];
    let mut app = app(m.clone(), d, None, true);
    app.panel = app.panel.take().map(|mut panel| {
        panel.pick_for_test(&["j.park", "nina.k"], &["bug", "webhooks", "flaky-test"]);
        panel
    });
    app
}

/// Frame 88: approved and ready, on a branch with a merge queue.
pub(crate) fn merge_app(confirming: Option<&str>) -> PrCraftApp {
    let m = ready(first());
    let mut d = detail(m.clone());
    d.queue = Some(MergeQueue { ahead: 3, estimate_secs: Some(480), queued: false });
    d.closes = vec![LinkedIssue { number: 471, title: "Retries bunch at the same second".into() }];
    let mut app = app(m, d, None, false);
    if let (Some(which), Some(panel)) = (confirming, app.panel.as_mut()) {
        panel.confirming(which);
    }
    app
}

/// Frame 89: the branch conflicts with main.
pub(crate) fn conflict_app() -> PrCraftApp {
    let mut m = first();
    m.merge = MergeState::Conflicts;
    app(m.clone(), detail(m), None, false)
}

/// Frame 94: the log of the failing job.
pub(crate) fn checks_app(confirming: bool) -> PrCraftApp {
    let mut app = inbox_app();
    let change: ChangeRef = first().change.change_ref();
    let mut screen = ChecksScreen::fixture(change, check_list(), "3", log());
    if confirming {
        screen.confirm_rerun();
    }
    app.checks = Some(screen);
    app
}

#[test]
fn your_changes_lists_each_change_with_its_size_and_the_first_thing_to_know() {
    let harness = harness(failing_app(), 1440.0, 900.0);
    assert!(says(&harness, "Your changes  3"), "the sidebar row carries the count");
    for text in
        ["Use crypto/rand for retry jitter", "Fix lint in invoice exporter", "Drop the legacy finalize path", "plus 9 minus 3", "plus 12 minus 140"]
    {
        assert!(says(&harness, text), "{text} is on screen");
    }
    assert!(says(&harness, "Checks failed · test:unit"), "a failing change says which check");
    assert!(says(&harness, "Approved · waiting to merge"));
    assert!(says(&harness, "Checks passing · waiting for review"));
}

#[test]
fn the_babysit_card_says_it_is_not_built_and_draws_no_control() {
    let harness = harness(failing_app(), 1440.0, 900.0);
    assert!(says(&harness, "Babysit is not built yet"));
    for control in ["Off", "Ask", "Auto", "Start babysitting"] {
        assert!(
            !named(&harness, Role::Button, control) && !named(&harness, Role::RadioButton, control),
            "no {control:?} control for a feature that is not built"
        );
    }
    assert!(named(&harness, Role::Button, "View log"), "the checks banner opens the log");
}

#[test]
fn a_reviewer_who_asked_for_changes_is_named_with_the_threads_that_wait() {
    let harness = harness(author_app(), 1440.0, 900.0);
    assert!(says(&harness, "j.park requested changes 3h ago"));
    assert!(says(&harness, "Changes requested · 2 threads open"), "the row says it too");
    assert!(says(&harness, "2 threads wait on you. 1 resolved."));
    for tag in ["Waiting on you", "Resolved"] {
        assert!(says(&harness, tag), "{tag} tags a thread");
    }
    assert!(says(&harness, "retry.go:41"));
    assert!(named(&harness, Role::Button, "Ask j.park for another look"), "the reply the frame offers: ask the reviewer to look again");
}

#[test]
fn a_draft_offers_reviewers_and_labels_and_sends_nothing_until_a_button_is_pressed() {
    let harness = harness(draft_app(), 1440.0, 900.0);
    assert!(says(&harness, "This is a draft"));
    for who in ["j.park", "nina.k", "dev.sato"] {
        assert!(named(&harness, Role::CheckBox, who), "{who} can be picked");
    }
    assert!(!says(&harness, "maria, "), "the author cannot review their own change");
    assert!(named(&harness, Role::Button, "Ready for review · ask 2 people"), "the button says how many people it asks");
    assert!(named(&harness, Role::Button, "Keep as draft"));
    assert!(says(&harness, "sent to GitHub when you press either button"), "nothing is said to sync later or offline");
}

#[test]
fn merging_always_asks_and_the_question_names_the_method_and_the_branch() {
    let harness = harness(merge_app(None), 1440.0, 900.0);
    assert!(says(&harness, "Ready to merge"));
    assert!(says(&harness, "2 of 2 approvals · 14 of 14 checks"), "the counts are the forge's");
    assert!(says(&harness, "main uses a merge queue"), "a queue is shown because the forge reported one");
    assert!(says(&harness, "3 changes ahead · about 8 min"));
    assert!(says(&harness, "Merging closes #471"));
    assert!(named(&harness, Role::Button, "Add to merge queue"));
    let confirm = self::harness(merge_app(Some("merge")), 1440.0, 900.0);
    assert!(says(&confirm, "Merge into main with Squash and merge?"));
    assert!(says(&confirm, "A merge cannot be undone."));
    assert!(says(&confirm, "maria/jitter is deleted afterward."), "the branch it deletes is named");
    assert!(named(&confirm, Role::Button, "Merge into main"));
}

#[test]
fn no_queue_is_shown_when_the_forge_reports_none() {
    let m = ready(first());
    let plain = app(m.clone(), detail(m), None, false);
    let harness = harness(plain, 1440.0, 900.0);
    assert!(!says(&harness, "merge queue"), "a branch without a queue says nothing about one");
    assert!(!says(&harness, "Merging closes"), "a forge that names no linked issue gets no issue line");
    assert!(named(&harness, Role::Button, "Squash and merge"));
}

#[test]
fn a_conflict_says_what_is_not_known_and_offers_no_ai_fix() {
    let harness = harness(conflict_app(), 1440.0, 900.0);
    assert!(says(&harness, "This branch conflicts with main"));
    assert!(says(&harness, "Conflicts with main"), "the row says it too");
    assert!(says(&harness, "neither GitHub nor GitLab names the files that conflict"));
    assert!(says(&harness, "Drafting a fix with the AI engine is not built yet"));
    let pill = harness.get_by_label_contains("Draft a fix · not built yet");
    assert!(pill.accesskit_node().is_disabled(), "the fix that is not built is shown, and cannot be pressed");
}

#[test]
fn the_job_log_tints_failing_lines_and_marks_them_in_words() {
    let harness = harness(checks_app(false), 1440.0, 900.0);
    assert!(says(&harness, "13 of 14 passing") || says(&harness, "4 of 5 passing"));
    assert!(
        says(&harness, "line 8, failing: --- FAIL: TestRetry_Jitter (0.03s)"),
        "a failing line is named as one, so the tint is not the only sign"
    );
    assert!(says(&harness, "line 2: go mod download"));
    assert!(named(&harness, Role::Button, "Re-run job"));
    assert!(says(&harness, "No explanation yet"), "the AI explanation is not built, and the pane says so");
    let ask = self::harness(checks_app(true), 1440.0, 900.0);
    assert!(says(&ask, "Re-run test:unit?"), "a re-run asks first and names the job");
    assert!(named(&ask, Role::Button, "Re-run test:unit"));
}

#[test]
fn an_empty_or_partial_your_changes_list_is_never_an_all_clear() {
    let mut empty = inbox_app();
    empty.view = Inbox::Mine;
    empty.mine = Forge::ALL.map(|f| MineSync::fixture(f, Vec::new())).into();
    let all_read = harness(empty, 1440.0, 900.0);
    assert!(says(&all_read, "You have no open changes"));
    let mut broken = inbox_app();
    broken.view = Inbox::Mine;
    broken.mine = Forge::ALL.map(|f| MineSync::fixture(f, Vec::new())).into();
    broken.mine[1].state = Some(pr_craft_engine::mine::MineState::Failed { error: "gitlab.acme.dev: 401".into() });
    let broken = harness(broken, 1440.0, 900.0);
    assert!(!says(&broken, "You have no open changes"), "a source that failed may hold changes");
    assert!(says(&broken, "could not read your changes: gitlab.acme.dev: 401"));
    assert!(says(&broken, "Nothing listed yet"));
}

#[test]
fn a_truncated_or_stale_list_says_so() {
    let mut app = failing_app();
    if let Some(pr_craft_engine::mine::MineState::Listed { truncated, .. }) = app.mine[0].state.as_mut() {
        *truncated = true;
    }
    app.mine[1].stale = Some("offline".into());
    let harness = harness(app, 1440.0, 900.0);
    assert!(says(&harness, "returned its limit of"));
    assert!(says(&harness, "The last re-check of glab failed: offline"));
    assert!(says(&harness, "Your changes  3+"), "the sidebar count says a change may be missing");
}

#[test]
fn back_to_you_lists_an_own_change_a_reviewer_waits_on() {
    let mut app = crate::parity::back_app();
    let waiting = requested_changes(first());
    for back in &mut app.back {
        if let crate::BackFetch::Done(pr_craft_engine::turn::BackState::Listed { yours, .. }) = &mut back.sync
            && back.forge == Forge::GitHub
        {
            *yours = vec![waiting.clone()];
        }
    }
    app.selected = Some(waiting.change.url.clone());
    app.panel = Some(Panel::fixture(&waiting, detail(waiting.clone()), Some(threads()), Vec::new(), Vec::new()));
    let harness = harness(app, 1440.0, 900.0);
    assert!(says(&harness, "Your changes"), "the band is named");
    assert!(says(&harness, "A reviewer is waiting on you"));
    assert!(says(&harness, "Changes requested · 2 threads open"));
    assert!(says(&harness, "j.park requested changes"), "the reading pane carries the frame's copy");
}

#[test]
fn the_confirm_step_cannot_be_answered_inside_its_arming_time_by_a_second_press() {
    let m = ready(first());
    let mut panel = Panel::fixture(&m, detail(m.clone()), None, Vec::new(), Vec::new());
    let ctx = craft::egui::Context::default();
    panel.press(&ctx, &m);
    assert!(panel.is_confirming() && panel.sends() == 0, "the first press only asks");
    panel.press(&ctx, &m);
    panel.press(&ctx, &m);
    assert_eq!(panel.sends(), 0, "a double click, a held key or the same press again answers nothing yet");
    panel.age_confirm();
    panel.press(&ctx, &m);
    assert_eq!(panel.sends(), 1, "once the reader has had time, it goes");
}

#[test]
fn command_enter_on_the_focused_primary_button_only_opens_the_confirm() {
    let mut harness = harness(merge_app(None), 1440.0, 900.0);
    harness.run_steps(3);
    harness.get_by_label_contains("Add to merge queue").focus();
    harness.run_steps(2);
    harness.key_press_modifiers(craft::egui::Modifiers::COMMAND, craft::egui::Key::Enter);
    harness.run_steps(3);
    let panel = harness.state().panel.as_ref().expect("a pane");
    assert!(panel.is_confirming(), "the press asked");
    assert_eq!(panel.sends(), 0, "the button's click and the key are one press, and it only asks");
}

#[test]
fn a_letter_typed_in_the_commit_message_changes_nothing_else() {
    let mut harness = harness(merge_app(None), 1440.0, 900.0);
    harness.run_steps(3);
    harness.get_by_role(Role::MultilineTextInput).click();
    harness.run_steps(3);
    harness.get_by_role(Role::MultilineTextInput).type_text("by nd jk");
    harness.run_steps(3);
    let app = harness.state();
    assert!(app.view == Inbox::Mine, "b, n, d, y did not switch the queue");
    assert!(app.selected.is_some(), "j and k did not move the selection");
    assert!(app.panel.is_some());
}

#[test]
fn an_armed_merge_when_ready_is_shown_and_can_be_turned_off_whatever_blocks_the_merge() {
    let mut m = failing(ready(first()));
    m.auto_merge = true;
    let harness = harness(app(m.clone(), detail(m), None, false), 1440.0, 900.0);
    assert!(says(&harness, "Merge when ready · on"), "failing checks do not hide an armed merge");
    assert!(named(&harness, Role::Switch, "Merge when ready"), "and it has its switch");
    let mut conflict = first();
    conflict.merge = MergeState::Conflicts;
    conflict.auto_merge = true;
    let harness = self::harness(app(conflict.clone(), detail(conflict), None, false), 1440.0, 900.0);
    assert!(says(&harness, "Merge when ready · on"), "a conflict does not either");
}

#[test]
fn a_merge_queue_that_could_not_be_read_is_said_and_plain_merge_stays_off() {
    let m = ready(first());
    let mut d = detail(m.clone());
    d.queue_error = Some("HTTP 502".into());
    let harness = harness(app(m, d, None, false), 1440.0, 900.0);
    assert!(says(&harness, "Could not read whether main has a merge queue: HTTP 502"));
    let pill = harness
        .query_all(egui_kittest::kittest::By::new().role(Role::Button))
        .find(|node| node.accesskit_node().label().is_some_and(|label| label.starts_with("Squash and merge")))
        .expect("the primary button");
    assert!(pill.accesskit_node().is_disabled(), "merging is off until the queue is known");
}
