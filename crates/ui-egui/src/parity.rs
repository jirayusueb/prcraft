//! The inbox and the review screen drawn offscreen at the size of the Figma frames (01 Inbox, 02 Review), with invented data
//! that mirrors the frames' sample so the two can be laid side by side. Everything here is test-only: a fixture app has no sync
//! thread and reads nothing from this Mac.
//!
//! The tests that read the accessibility tree always run. The ones that draw pixels need a wgpu adapter, so they are behind
//! the `renders` feature and write PNGs to `.egm/parity/app` (or the directory `PR_CRAFT_RENDERS` names):
//!
//! ```sh
//! cargo test -p pr-craft-ui-egui --features renders --lib parity::renders
//! ```

use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use craft::egui::{self, Context, Role, Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use pr_craft_core::agent::{Agent, Engine, EngineSettings, RunsOn, SpawnLimits, Trigger};
use pr_craft_core::commits::{Commit, Picked};
use pr_craft_core::diff::{FileDiff, FileStatus, parse_unified};
use pr_craft_core::diff_view::LayoutOptions;
use pr_craft_core::review::{Anchor, Draft, LineRef, LocalReview, Verdict};
use pr_craft_core::run::{AiDraft, DraftState, Finding, RunState};
use pr_craft_core::stack::StackPlace;
use pr_craft_core::thread::{Original, ReviewThread, ThreadComment};
use pr_craft_core::turn::{Assessment, MyReview, Reason, ReviewState, Turn};
use pr_craft_core::{Change, ChangeRef, Forge};
use pr_craft_engine::ai::{EngineInfo, EngineView};
use pr_craft_engine::commits::{CommitList, Reviewed};
use pr_craft_engine::review::ReviewView;
use pr_craft_engine::thread::ThreadList;
use pr_craft_engine::turn::{BackItem, BackState, Event, EventKind, Notifications, SinceLast};
use pr_craft_forge::review::ChangeMeta;

use crate::activity::Activity;
use crate::agents::AgentsPage;
use crate::ai::tests::run;
use crate::ai::{RunFilter, Runs};
use crate::review::ReviewScreen;
use crate::settings::{Settings, Tab};
use crate::sheet::AiSheet;
use crate::theme::{self, Look};
use crate::{BackFetch, BackSync, Inbox, Overlay, PrCraftApp};

fn change(forge: Forge, repo: &str, number: u64, title: &str, author: &str, hours_ago: i64) -> Change {
    let host = match forge {
        Forge::GitHub => "github.com",
        Forge::GitLab => "gitlab.acme.dev",
    };
    Change {
        forge,
        repo: repo.into(),
        number,
        title: title.into(),
        author: author.into(),
        url: format!("https://{host}/{repo}/{number}"),
        updated_at: Utc::now() - Duration::hours(hours_ago) - Duration::minutes(number as i64 % 7),
        draft: false,
        host: None,
        stack: None,
    }
}

/// The rows of Figma frame 01: a lone change, a stack of four, and the rest.
fn inbox_changes() -> Vec<Change> {
    let stacked = |number: u64, title: &str, draft: bool| {
        let index = (number as usize - 1291) / 2;
        let stack = StackPlace { bottom: 1291, top: 1297, index, size: 4 };
        Change { draft, stack: Some(stack), ..change(Forge::GitLab, "billing-service", number, title, "t.okafor", 3) }
    };
    vec![
        change(Forge::GitHub, "acme/api", 482, "Retry idempotent webhooks with exponential backoff", "nina.k", 2),
        stacked(1297, "Drop the legacy finalize path", true),
        stacked(1295, "Void takes the same invoice lock", false),
        stacked(1293, "Finalize invoices in a single transaction", false),
        stacked(1291, "Add the invoice_locks table", false),
        change(Forge::GitHub, "acme/web", 2210, "Move settings page to the new form primitives", "dev.sato", 5),
        change(Forge::GitLab, "infra-modules", 88, "Pin Terraform AWS provider to 5.x", "r.amini", 6),
        change(Forge::GitHub, "acme/ios", 731, "Fix crash when the share sheet closes mid-upload", "lea.b", 26),
        change(Forge::GitHub, "acme/api", 479, "Add cursor pagination to /v2/events", "nina.k", 27),
        change(Forge::GitHub, "acme/web", 2198, "Bump Vite to 6 and fix SSR externals", "renovate[bot]", 50),
        change(Forge::GitLab, "billing-service", 1279, "Expose dunning schedule in the admin API", "m.chen", 74),
    ]
}

pub(crate) fn inbox_app() -> PrCraftApp {
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, Runs::default());
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app
}

const INVOICER_DIFF: &str = "diff --git a/internal/ledger/invoicer.go b/internal/ledger/invoicer.go
--- a/internal/ledger/invoicer.go
+++ b/internal/ledger/invoicer.go
@@ -36,11 +36,16 @@ func (s *Invoicer) Finalize(ctx context.Context, id InvoiceID) error {
 func (s *Invoicer) Finalize(ctx context.Context, id InvoiceID) error {
 	inv, err := s.repo.Get(ctx, id)
 	if err != nil {
 		return fmt.Errorf(\"load invoice %s: %w\", id, err)
 	}
-	if err := s.repo.MarkFinal(ctx, inv); err != nil {
-		return err
-	}
-	return s.ledger.Post(ctx, inv.Lines())
+	return s.repo.WithTx(ctx, func(tx Tx) error {
+		if err := tx.LockInvoice(ctx, id); err != nil {
+			return fmt.Errorf(\"lock invoice %s: %w\", id, err)
+		}
+		if err := tx.MarkFinal(ctx, inv); err != nil {
+			return err
+		}
+		return s.ledger.PostTx(ctx, tx, inv.Lines())
+	})
 }
 
@@ -71,6 +76,9 @@ func (s *Invoicer) Void(ctx context.Context, id InvoiceID) error {
 	if err != nil {
 		return err
 	}
+	if err := s.repo.LockInvoice(ctx, id); err != nil {
+		return err
+	}
 	inv.Void()
 	return s.repo.Save(ctx, inv)
 }
";

fn plain_file(path: &str, status: FileStatus, additions: u32, deletions: u32) -> FileDiff {
    FileDiff {
        path: path.into(),
        old_path: None,
        status,
        hunks: Vec::new(),
        additions,
        deletions,
        binary: false,
        generated: false,
        hash: additions as u64,
    }
}

/// Figma frame 02: four files, three drafts, one AI suggestion under the closing brace of `Finalize`.
fn review_screen(from_back: bool) -> (ReviewScreen, Runs) {
    review_screen_with(from_back, true)
}

/// [`review_screen`]; without `inline`, no draft or AI suggestion sits under a line of the first file.
fn review_screen_with(from_back: bool, inline: bool) -> (ReviewScreen, Runs) {
    let change = ChangeRef { forge: Forge::GitHub, repo: "acme/billing-service".into(), number: 1293, host: None };
    let anchor = Anchor { head_sha: "abc".into(), base_sha: None, start_sha: None };
    let mut files = parse_unified(INVOICER_DIFF);
    files.push(plain_file("internal/ledger/invoicer_test.go", FileStatus::Modified, 61, 4));
    files.push(plain_file("db/migrations/0087_invoice_lock.sql", FileStatus::Added, 9, 0));
    files.push(plain_file("internal/ledger/tx.go", FileStatus::Modified, 4, 28));
    let draft = |id: u64, path: &str, line: u32, body: &str| Draft {
        id,
        path: path.into(),
        old_path: None,
        start: None,
        end: LineRef { old: None, new: Some(line) },
        body: body.into(),
        on_rendered: false,
    };
    let viewed: BTreeMap<String, u64> =
        files.iter().filter(|f| f.path.ends_with("_test.go") || f.path.ends_with("tx.go")).map(|f| (f.path.clone(), f.hash)).collect();
    let mut local = LocalReview {
        drafts: vec![
            draft(
                1,
                "internal/ledger/invoicer.go",
                44,
                "inv is loaded before the lock is taken, so a concurrent Void can still finalize\na stale invoice. Can the Get move inside WithTx, after LockInvoice?",
            ),
            draft(2, "internal/ledger/invoicer_test.go", 112, "No rollback test for ledger error"),
            draft(3, "db/migrations/0087_invoice_lock.sql", 3, "Add a note on migration 0087"),
        ],
        verdict: Verdict::RequestChanges,
        body: String::new(),
        viewed,
        next_id: 4,
        anchor: Some(anchor.clone()),
        post_unknown: None,
        extra: Default::default(),
    };
    if !inline {
        // Frame 90 shows threads under the lines; the drafts that sit on other files stay in Your review.
        local.drafts.retain(|d| d.path != "internal/ledger/invoicer.go");
    }
    let meta = ChangeMeta {
        title: "Finalize invoices in a single transaction".into(),
        author: "t.okafor".into(),
        url: "https://github.com/acme/billing-service/pull/1293".into(),
        head_branch: "tokafor/finalize-tx".into(),
        base_branch: "main".into(),
        head_sha: "abc".into(),
        base_sha: None,
        diff_refs: None,
    };
    let view = ReviewView { change: change.clone(), meta, anchor, stale: false, files, local, filter: Vec::new(), layout: Default::default() };
    let mut suggestion = run("r", 1293, RunState::Done, Utc::now() - Duration::minutes(3));
    suggestion.change = change;
    suggestion.agent.name = "Claude Code".into();
    suggestion.drafts = vec![AiDraft {
        id: "r/0".into(),
        finding: Finding {
            path: "internal/ledger/invoicer.go".into(),
            old_path: None,
            line: LineRef { old: Some(45), new: Some(50) },
            body: "PostTx runs while the invoice row is locked, so a slow ledger call blocks Void.\nWrite to an outbox inside the transaction and post after commit?".into(),
            agent: "Claude Code".into(),
        },
        state: DraftState::Pending,
    }];
    (ReviewScreen::fixture(view, Vec::new(), from_back), if inline { Runs::with(vec![suggestion]) } else { Runs::default() })
}

fn agent(id: &str, name: &str, description: &str, engine: Engine, skills: &[&str], scope: &[&str]) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        prompt: "You review Go changes for concurrency bugs.\nFor each finding, cite the file and line, say what can go wrong,\nand propose the smallest fix. Skip style issues.\n\nFocus on: data races, lock ordering, goroutine leaks,\ncontext cancellation, and shared state across requests.\n\nChange under review: {pr_title}\nFiles in your scope: {files}".into(),
        skills: skills.iter().map(|s| (*s).to_string()).collect(),
        engine,
        scope: scope.iter().map(|s| (*s).to_string()).collect(),
        triggers: vec![Trigger::ReviewRequested],
        spawn: SpawnLimits::default(),
        runs_on: RunsOn::All,
        extra: Default::default(),
    }
}

fn agents() -> Vec<Agent> {
    vec![
        agent("security", "Security reviewer", "Secrets, injection, authz checks", Engine::ClaudeCode, &["a", "b", "c"], &["**/*"]),
        agent("idioms", "Go idioms", "Errors, context, naming", Engine::ZaiGlm, &["a"], &["**/*.go"]),
        agent("migrations", "Migration safety", "Locks and backfills in db/**", Engine::ClaudeCode, &["a", "b"], &["db/**"]),
        agent("gaps", "Test gaps", "Untested branches in the diff", Engine::ClaudeCode, &[], &["**/*_test.go"]),
    ]
}

fn engine_view() -> EngineView {
    let info = |engine: Engine, program: Option<&str>| EngineInfo {
        engine,
        name: engine.name().into(),
        wired: engine.wired(),
        program: program.map(String::from),
        note: None,
    };
    EngineView {
        settings: EngineSettings::default(),
        engines: vec![info(Engine::ClaudeCode, Some("~/.local/bin/claude")), info(Engine::ZaiGlm, None)],
    }
}

/// Frame 12: a new agent, half written.
fn agent_new_app() -> PrCraftApp {
    let mut page = AgentsPage::fixture(agents());
    page.fixture_new_form("Concurrency reviewer", "Looks for races, lock order and shared state in Go changes.");
    let mut app = inbox_app();
    app.overlay = Overlay::Settings(Box::new(Settings::fixture(Tab::Agents, engine_view(), page)));
    app
}

fn settings_app(tab: Tab) -> PrCraftApp {
    let mut app = inbox_app();
    app.overlay = Overlay::Settings(Box::new(Settings::fixture(tab, engine_view(), AgentsPage::fixture(agents()))));
    app
}

/// Frame 21: four changes back to the reviewer and three with their authors.
fn back_items() -> Vec<BackItem> {
    let item = |forge: Forge, repo: &str, number: u64, title: &str, author: &str, hours: i64, state: ReviewState, turn: Turn| {
        let change = change(forge, repo, number, title, author, hours);
        let review = MyReview { state, submitted_at: Some(Utc::now() - Duration::hours(hours + 48)), commit: None };
        BackItem { change, assessment: Assessment { review, commits_since: 2, turn } }
    };
    let back = |reasons: Vec<Reason>| Turn::BackToYou { reasons };
    vec![
        item(
            Forge::GitHub,
            "acme/api",
            482,
            "Retry idempotent webhooks with backoff",
            "nina.k",
            24,
            ReviewState::ChangesRequested,
            back(vec![Reason::AuthorReplied { count: 3, last_reply: Utc::now() }, Reason::NewCommits { count: 2 }]),
        ),
        item(
            Forge::GitLab,
            "infra-modules",
            88,
            "Pin Terraform AWS provider to 5.x",
            "r.amini",
            6,
            ReviewState::Approved,
            back(vec![Reason::ApprovalDismissed]),
        ),
        item(
            Forge::GitHub,
            "acme/api",
            479,
            "Add cursor pagination to /v2/events",
            "nina.k",
            48,
            ReviewState::Commented,
            back(vec![Reason::ReRequested { at: None }]),
        ),
        item(
            Forge::GitLab,
            "billing-service",
            1293,
            "Finalize invoices in one transaction",
            "m.chen",
            3,
            ReviewState::ChangesRequested,
            back(vec![Reason::ThreadsResolved { count: 2 }]),
        ),
        item(
            Forge::GitHub,
            "acme/web",
            2210,
            "Move settings page to the new form primitives",
            "dev.sato",
            26,
            ReviewState::ChangesRequested,
            Turn::WithAuthor,
        ),
        item(Forge::GitHub, "acme/ios", 731, "Fix crash when the share sheet closes", "lea.b", 5, ReviewState::Commented, Turn::WithAuthor),
        item(
            Forge::GitLab,
            "billing-service",
            1279,
            "Expose dunning schedule in the admin API",
            "j.park",
            74,
            ReviewState::Approved,
            Turn::WithAuthor,
        ),
    ]
}

pub(crate) fn back_app() -> PrCraftApp {
    let items = back_items();
    let selected = items[0].change.url.clone();
    let mut app = inbox_app();
    app.view = Inbox::BackToYou;
    app.selected = Some(selected);
    app.back = Forge::ALL
        .map(|forge| BackSync {
            forge,
            sync: BackFetch::Done(BackState::Listed {
                items: items.iter().filter(|i| i.change.forge == forge).cloned().collect(),
                truncated: false,
                incomplete: None,
                yours: Vec::new(),
            }),
            refresh: None,
            stale: None,
        })
        .into();
    app
}

/// Frame 07: five events, three of them new.
fn notifications_app() -> PrCraftApp {
    let events = [
        (Forge::GitLab, "billing-service", 1293, "t.okafor asked you to review", 3, false, EventKind::ReviewRequested),
        (Forge::GitLab, "billing-service", 1291, "Stack ready: !1291 was approved", 3, false, EventKind::BackToYou),
        (Forge::GitHub, "acme/api", 479, "nina.k pushed 4 commits after your review", 5, false, EventKind::BackToYou),
        (Forge::GitHub, "acme/api", 488, "j.park approved your pull request", 26, true, EventKind::BackToYou),
        (Forge::GitLab, "infra-modules", 88, "r.amini mentioned you", 70, true, EventKind::BackToYou),
    ]
    .map(|(forge, repo, number, text, hours, seen, kind)| {
        let change = change(forge, repo, number, "A change", "t.okafor", hours);
        Event { id: format!("e{number}"), kind, change, text: text.into(), at: Utc::now() - Duration::hours(hours), seen }
    });
    let mut app = inbox_app();
    app.view = Inbox::Notifications;
    app.selected = Some(events[0].id.clone());
    app.notifications = Some(Notifications {
        unseen: 3,
        events: events.into(),
        errors: Vec::new(),
        pause: None,
        system_notice: pr_craft_engine::turn::SYSTEM_NOTICE,
    });
    app
}

/// Frame 22: two commits after the reviewer's last look, two files.
fn since_app() -> PrCraftApp {
    let (screen, runs) = review_screen(true);
    let files = parse_unified(INVOICER_DIFF);
    let since = SinceLast {
        change: ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 482, host: None },
        review: MyReview { state: ReviewState::ChangesRequested, submitted_at: Some(Utc::now() - Duration::days(2)), commit: None },
        from: "a41c9e".into(),
        to: "7be210".into(),
        commits_since: 2,
        files,
        omitted: Vec::new(),
        rewritten: false,
        truncated: false,
        web_url: None,
    };
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, runs);
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app.review = Some(screen.with_since(since));
    app
}

/// Frame 29: three agents and a general pass over four files.
fn sheet_app() -> PrCraftApp {
    let (screen, runs) = review_screen(false);
    let go = ["billing/invoicer.go", "billing/invoicer_test.go", "billing/tx.go"];
    let all = ["billing/invoicer.go", "billing/invoicer_test.go", "billing/tx.go", "migrations/0087_invoice_lock.sql"];
    let plan = crate::ai::tests::plan(&[("Concur.", &go), ("Security", &all), ("Migr.", &["migrations/0087_invoice_lock.sql"])], &all, &[]);
    let agent = |id: &str, name: &str, scope: &[&str]| Agent { id: id.into(), ..agent(id, name, "", Engine::ClaudeCode, &[], scope) };
    let agents = vec![
        agent("Concur.", "Concurrency reviewer", &["**/*.go"]),
        agent("Security", "Security reviewer", &["**/*"]),
        agent("Migr.", "Migration safety", &["migrations/**"]),
    ];
    let change = ChangeRef { forge: Forge::GitHub, repo: "acme/billing-service".into(), number: 1293, host: None };
    let mut app = inbox_app();
    app.runs = runs;
    app.review = Some(screen.with_sheet(AiSheet::fixture(change, plan, agents)));
    app
}

/// Frame 61: one run looks stuck among others that go.
fn activity_app() -> PrCraftApp {
    let started = |minutes: i64| Utc::now() - Duration::minutes(minutes);
    let mut stuck = run("stuck", 1293, RunState::LooksStuck, started(8));
    stuck.change = ChangeRef { forge: Forge::GitLab, repo: "billing-service".into(), number: 1293, host: None };
    stuck.change_title = "Finalize invoices in a single transaction".into();
    stuck.agent.name = "Migration safety".into();
    stuck.pid = Some(48211);
    stuck.last_output = Some(started(6));
    stuck.tail = vec![
        "10:35:48  read migrations/0087_invoice_lock.sql".into(),
        "10:35:55  ran: psql --dry-run … (exit 0)".into(),
        "10:36:01  asked: \"is the invoice_locks index concurrent?\"".into(),
        "10:36:01  waiting for tool result …".into(),
    ];
    let others =
        [run("a", 482, RunState::Running, started(2)), run("b", 479, RunState::Running, started(1)), run("c", 731, RunState::Done, started(30))];
    let runs = Runs::with(std::iter::once(stuck).chain(others).collect());
    let mut app = inbox_app();
    app.runs = runs;
    app.overlay = Overlay::Activity(Box::new(Activity::fixture(RunFilter::Stuck, "stuck")));
    app
}

/// Frame 19: the share sheet over the review.
fn share_app() -> PrCraftApp {
    let (screen, runs) = review_screen(false);
    let mut app = inbox_app();
    app.runs = runs;
    app.review = Some(screen.with_share());
    app
}

const RETRY_DIFF: &str = "diff --git a/internal/webhooks/retry.go b/internal/webhooks/retry.go
--- a/internal/webhooks/retry.go
+++ b/internal/webhooks/retry.go
@@ -30,7 +30,12 @@ func (r *Retrier) Deliver(d Delivery) error {
 func (r *Retrier) Deliver(d Delivery) error {
-\tn  := r.attempts(d.ID)
+\tn := r.attempts(d.ID)
-\tif n > 3 {
+\tif n >= r.cfg.MaxAttempts {
 \t\treturn ErrGaveUp
 \t}
-\ttime.Sleep(time.Minute)
+\tdelay := backoff(n, base, max)
+\tselect {
+\tcase <-time.After(delay):
+\tcase <-ctx.Done():
+\t\treturn ctx.Err()
+\t}
 \treturn r.post(ctx, d)
";

/// Figma frame 91: six files, the first drawn in split view with whitespace hidden and changed words marked.
fn retry_view() -> ReviewView {
    let change = ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 482, host: None };
    let anchor = Anchor { head_sha: "abc".into(), base_sha: None, start_sha: None };
    let mut files = parse_unified(RETRY_DIFF);
    for (path, status, adds, dels) in [
        ("internal/webhooks/worker.go", FileStatus::Modified, 41, 19),
        ("internal/webhooks/retry_test.go", FileStatus::Modified, 31, 4),
        ("db/migrations/0142_webhook_attempts.sql", FileStatus::Added, 9, 0),
        ("docs/webhooks.md", FileStatus::Modified, 7, 5),
        ("internal/config/config.go", FileStatus::Modified, 3, 1),
    ] {
        files.push(plain_file(path, status, adds, dels));
    }
    let draft = Draft {
        id: 1,
        path: "internal/webhooks/retry.go".into(),
        old_path: None,
        start: None,
        end: LineRef { old: None, new: Some(35) },
        body: "backoff cap is hard-coded".into(),
        on_rendered: false,
    };
    let local = LocalReview { drafts: vec![draft], next_id: 2, anchor: Some(anchor.clone()), ..LocalReview::default() };
    let meta = ChangeMeta {
        title: "Retry idempotent webhooks with exponential backoff".into(),
        author: "nina.k".into(),
        url: "https://github.com/acme/api/pull/482".into(),
        head_branch: "nina/webhook-retries".into(),
        base_branch: "main".into(),
        head_sha: "abc".into(),
        base_sha: None,
        diff_refs: None,
    };
    ReviewView { change, meta, anchor, stale: false, files, local, filter: Vec::new(), layout: Default::default() }
}

fn split_screen(options_open: bool) -> (ReviewScreen, Runs) {
    let layout = LayoutOptions { split: true, hide_whitespace: true, word_highlight: true, wrap: false, extra: Default::default() };
    (ReviewScreen::fixture(retry_view(), Vec::new(), false).with_layout(layout, options_open), Runs::default())
}

fn split_app(options_open: bool) -> PrCraftApp {
    let (screen, runs) = split_screen(options_open);
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, runs);
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app.review = Some(screen);
    app
}

/// The review `screen` over the inbox, with its runs.
fn review_over(screen: ReviewScreen, runs: Runs) -> PrCraftApp {
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, runs);
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app.review = Some(screen);
    app
}

/// Figma frame 79: `src/icons/retry.svg` before and after (412 B and 538 B, the arrow drawn with 31 and 46 points), and its diff.
const RETRY_SVG_OLD: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#1d1c21" stroke-linecap="round" stroke-linejoin="round">
  <path id="arrow" fill="none"
      d="M4 12a8 8 0 1 0 2.3-5.7M4 12l-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25M4 12l.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25"
      stroke-width="1.5"
  />
</svg>
"##;
const RETRY_SVG_NEW: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#1d1c21" stroke-linecap="round" stroke-linejoin="round">
  <path fill="none" id="arrow"
        d="M3 11a9 9 0 0 1 15-5.3v-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2M18 5.7h-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2" stroke-width="2"/>
  <path id="tail" data-name="second arrow" stroke-width="2"
        d="M21 13a9 9 0 0 1-15 5.3v.75 .75 .75 .75 .75 .75M6 18.3h.7 .7 .7 .7 .7 .7"/>
  <title>Retry</title>

</svg>
"##;
const RETRY_SVG_DIFF: &str = r##"diff --git a/src/icons/retry.svg b/src/icons/retry.svg
--- a/src/icons/retry.svg
+++ b/src/icons/retry.svg
@@ -1,6 +1,8 @@
 <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#1d1c21" stroke-linecap="round" stroke-linejoin="round">
-  <path id="arrow" fill="none"
-      d="M4 12a8 8 0 1 0 2.3-5.7M4 12l-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25-.25.25M4 12l.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25.3.25"
-      stroke-width="1.5"
-  />
+  <path fill="none" id="arrow"
+        d="M3 11a9 9 0 0 1 15-5.3v-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2M18 5.7h-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2-.2" stroke-width="2"/>
+  <path id="tail" data-name="second arrow" stroke-width="2"
+        d="M21 13a9 9 0 0 1-15 5.3v.75 .75 .75 .75 .75 .75M6 18.3h.7 .7 .7 .7 .7 .7"/>
+  <title>Retry</title>
+
 </svg>
"##;

/// Figma frame 79: an icon change drawn before and after, what changed in its markup, and a draft written on the rendered view.
fn svg_app() -> PrCraftApp {
    let change = ChangeRef { forge: Forge::GitHub, repo: "acme/web".into(), number: 2231, host: None };
    let anchor = Anchor { head_sha: "abc".into(), base_sha: None, start_sha: None };
    let mut files = parse_unified(RETRY_SVG_DIFF);
    files.push(plain_file("src/icons/sync.svg", FileStatus::Modified, 3, 3));
    files.push(plain_file("src/icons/index.ts", FileStatus::Modified, 1, 0));
    let viewed = files.iter().filter(|f| f.path.ends_with("sync.svg")).map(|f| (f.path.clone(), f.hash)).collect();
    let draft = Draft {
        id: 1,
        path: "src/icons/retry.svg".into(),
        old_path: None,
        start: None,
        end: LineRef { old: None, new: Some(3) },
        body: "At 16 px the two arrowheads merge into one blob. Check it in the toolbar at 1×.".into(),
        on_rendered: true,
    };
    let local = LocalReview { drafts: vec![draft], viewed, next_id: 2, anchor: Some(anchor.clone()), ..LocalReview::default() };
    let meta = ChangeMeta {
        title: "Use the two-arrow retry icon".into(),
        author: "dev.sato".into(),
        url: "https://github.com/acme/web/pull/2231".into(),
        head_branch: "dev.sato/retry-icon".into(),
        base_branch: "main".into(),
        head_sha: "abc".into(),
        base_sha: None,
        diff_refs: None,
    };
    let view = ReviewView { change, meta, anchor, stale: false, files, local, filter: Vec::new(), layout: Default::default() };
    let screen = ReviewScreen::fixture(view, Vec::new(), false).with_svg("src/icons/retry.svg", RETRY_SVG_OLD, RETRY_SVG_NEW);
    review_over(screen, Runs::default())
}

/// Figma frame 103: lines 42 to 44 of `invoicer.go` selected, ready for a right-click.
fn lines_app() -> PrCraftApp {
    let (screen, runs) = review_screen(false);
    review_over(screen.with_lines(0, 10, 12), runs)
}

/// [`lines_app`], with the second file current: the selection is in the first.
fn lines_app_on_other_file() -> PrCraftApp {
    let (screen, runs) = review_screen(false);
    review_over(screen.with_lines(0, 10, 12).with_current_file(1), runs)
}

/// [`tree_app`] with the docs folder renamed to one the Files filter cannot write.
fn spaced_folder_app() -> PrCraftApp {
    let mut view = retry_view();
    for file in view.files.iter_mut().filter(|f| f.path.starts_with("docs/")) {
        file.path = file.path.replacen("docs/", "docs/My Notes/", 1);
    }
    review_over(ReviewScreen::fixture(view, Vec::new(), false), Runs::default())
}

/// Figma frame 105: six files in unified view, two of them viewed, ready for a right-click on a folder.
fn tree_app() -> PrCraftApp {
    let mut view = retry_view();
    let viewed: Vec<(String, u64)> = view
        .files
        .iter()
        .filter(|f| f.path.ends_with("retry_test.go") || f.path.ends_with("0142_webhook_attempts.sql"))
        .map(|f| (f.path.clone(), f.hash))
        .collect();
    view.local.viewed.extend(viewed);
    review_over(ReviewScreen::fixture(view, Vec::new(), false), Runs::default())
}

/// Figma frame 107, in the place the app has for it: a change with no draft on this Mac.
fn no_drafts_app() -> PrCraftApp {
    let (screen, runs) = review_screen_with(false, false);
    review_over(screen.with_no_drafts(), runs)
}

/// Figma frame 108: every file of the change marked viewed.
fn all_viewed_app() -> PrCraftApp {
    let (screen, runs) = review_screen(false);
    review_over(screen.with_every_file_viewed(), runs)
}

/// Figma frame 106: the search that finds nothing.
fn search_empty_app() -> PrCraftApp {
    let mut app = inbox_app();
    app.palette = Some(crate::palette::Palette::typed("jitterbug"));
    app
}

/// Figma frame 109: the Notifications list with nothing in it and every source answered.
fn caught_up_app() -> PrCraftApp {
    let mut app = inbox_app();
    app.view = Inbox::Notifications;
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 0, errors: Vec::new(), pause: None, system_notice: "" });
    app
}

/// Figma frame 110: six runs finished today and none going.
fn idle_activity_app() -> PrCraftApp {
    let runs: Vec<_> = (0..6).map(|n| run(&format!("done{n}"), 480 + n, RunState::Done, Utc::now() - Duration::minutes(2 + n as i64))).collect();
    let mut app = inbox_app();
    app.runs = Runs::with(runs);
    app.overlay = Overlay::Activity(Box::new(Activity::fixture(RunFilter::Running, "")));
    app
}

/// Figma frame 107 and the Local drafts row of frame 01: the queue with three changes, or with none.
fn local_drafts_app(any: bool) -> PrCraftApp {
    let mut app = inbox_app();
    let change = |forge: Forge, repo: &str, number: u64| ChangeRef { forge, repo: repo.into(), number, host: None };
    let held = if any {
        vec![
            crate::drafts::pending(change(Forge::GitHub, "acme/api", 482), 2, Verdict::Comment),
            crate::drafts::pending(change(Forge::GitLab, "billing-service", 1293), 3, Verdict::RequestChanges),
            crate::drafts::pending(change(Forge::GitHub, "acme/ios", 731), 1, Verdict::Comment),
        ]
    } else {
        Vec::new()
    };
    crate::drafts::fixture_drafts(&mut app, held, Vec::new());
    app
}

/// An invented thread comment `hours` old.
fn comment(author: &str, hours: i64, body: &str) -> ThreadComment {
    ThreadComment { author: author.into(), at: Some(Utc::now() - Duration::hours(hours)), body: body.into() }
}

/// Figma frame 90: the `invoicer.go` diff with an open thread on line 44, a resolved one, and an outdated one.
fn thread_screen() -> (ReviewScreen, Runs) {
    let (screen, runs) = review_screen_with(false, false);
    let thread = |id: &str, line: Option<u32>, comments: Vec<ThreadComment>| ReviewThread {
        id: id.into(),
        path: "internal/ledger/invoicer.go".into(),
        line: line.map(|n| LineRef { old: None, new: Some(n) }),
        original: Original::default(),
        resolved: false,
        resolved_by: None,
        outdated: line.is_none(),
        can_resolve: true,
        can_reply: true,
        comments_total: comments.len() as u32,
        comments,
    };
    let open = thread(
        "t1",
        Some(44),
        vec![
            comment("You", 24, "Post the ledger entry after commit, not inside the transaction."),
            comment("t.okafor", 3, "Moved it to an outbox row; the poster runs after commit."),
        ],
    );
    let resolved = ReviewThread {
        resolved: true,
        resolved_by: Some("t.okafor".into()),
        ..thread("t3", Some(39), vec![comment("You", 30, "Wrap this with the invoice id.")])
    };
    let outdated = ReviewThread {
        original: Original { commit: Some("a41c9e7d".into()), line: Some(52), code: Vec::new() },
        ..thread("t2", None, vec![comment("You", 48, "Why not reuse LockInvoice here?")])
    };
    let change = ChangeRef { forge: Forge::GitHub, repo: "acme/billing-service".into(), number: 1293, host: None };
    let list = ThreadList { change, threads: vec![open, resolved, outdated], complete: true, note: None };
    (screen.with_threads(list), runs)
}

fn threads_app() -> PrCraftApp {
    let (screen, runs) = thread_screen();
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, runs);
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app.review = Some(screen);
    app
}

/// Figma frame 95: six commits, the last two picked as "since your review".
fn commits_app() -> PrCraftApp {
    let (screen, runs) = review_screen(false);
    let ago = |hours: i64| Some(Utc::now() - Duration::hours(hours));
    let rows = [
        ("e5c1b08", "Draft finalize rewrite", 48),
        ("0d77e35", "Add tx.go", 47),
        ("91fa2c4", "Wrap finalize in WithTx", 26),
        ("3be0d71", "Add LockInvoice helper", 25),
        ("a41c9e2", "Take the lock before the read", 5),
        ("7c2e1f0", "Move ledger post to outbox", 3),
    ];
    let mut commits: Vec<Commit> = Vec::new();
    for (sha, title, hours) in rows {
        let parent = commits.last().map(|c| c.sha.clone());
        commits.push(Commit { sha: format!("{sha}aaaa"), title: title.into(), author: "t.okafor".into(), at: ago(hours), parent, merge: false });
    }
    let change = ChangeRef { forge: Forge::GitHub, repo: "acme/billing-service".into(), number: 1293, host: None };
    let reviewed = Reviewed { commit: "3be0d71aaaa".into(), state: ReviewState::ChangesRequested, submitted_at: ago(24) };
    let list = CommitList { change, commits, complete: true, reviewed: Some(reviewed), reviewed_error: None };
    let mut app = PrCraftApp::fixture(inbox_changes(), &inbox_changes()[0].url, runs);
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app.review = Some(screen.with_commits(list, Some(Picked { first: 4, last: 5 })));
    app
}

/// What the Open locally popover shows: which clone is registered and whether the worktree is made.
enum LocalCase {
    /// The clone is registered and clean, the worktree was made earlier.
    Ready,
    /// "Check out here" waits for the reader's yes.
    Confirm,
    /// Nothing registered yet.
    NoClone,
    /// A clone with work in it: "Check out here" refuses and says why.
    Dirty,
}

/// Figma frame 96: the popover under Open, over the review with its first line selected. The engine's own `Status` is built by
/// hand, with folders under HOME so the popover writes them with `~`.
fn local_app(case: LocalCase) -> PrCraftApp {
    use pr_craft_core::checkout::{Action, LocalState};
    use pr_craft_engine::local::{CloneStatus, Status};
    let (screen, runs) = review_screen(false);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/maya".into());
    let change = ChangeRef { forge: Forge::GitHub, repo: "acme/billing-service".into(), number: 1293, host: None };
    let (clone, worktree) = (format!("{home}/code/billing-service"), format!("{home}/PR Craft checkouts/acme-billing-service-1293"));
    let state = LocalState {
        branch: Some("main".into()),
        head: Some("5d41402abc4b2a76b9719d911017c592".into()),
        changed: if matches!(case, LocalCase::Dirty) { vec!["billing/invoicer.go".into(), "go.mod".into()] } else { Vec::new() },
        ..LocalState::default()
    };
    let dirty = pr_craft_core::checkout::refuse_here(&state);
    let registered = !matches!(case, LocalCase::NoClone);
    let status = Status {
        change: change.clone(),
        checkouts_root: Some(format!("{home}/PR Craft checkouts")),
        clone: registered.then(|| CloneStatus { path: clone, remote: Some("origin".into()), state: Some(state), error: None }),
        worktree: registered.then(|| worktree.clone()),
        worktree_other_commit: false,
        worktree_path: Some(worktree.clone()),
        head_branch: Some("tokafor/finalize-tx".into()),
        editor_root: registered.then_some(worktree),
        worktree_action: if registered { Action::yes() } else { Action::no("No local clone of acme/billing-service is registered.") },
        here_action: match (registered, dirty) {
            (false, _) => Action::no("No local clone of acme/billing-service is registered."),
            (true, Some(why)) => Action::no(why),
            (true, None) => Action::yes(),
        },
        forge_error: None,
        setup: (!registered).then(|| {
            "No local clone of acme/billing-service is registered. In Settings → Repositories, choose acme/billing-service and set its local folder to a clone of it."
                .to_string()
        }),
    };
    let said = matches!(case, LocalCase::Ready).then(|| Ok("Reused the worktree at ~/PR Craft checkouts/acme-billing-service-1293.".to_string()));
    let mut local = crate::local::Local::fixture(status, said);
    if matches!(case, LocalCase::Confirm) {
        local = local.with_confirm();
    }
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, runs);
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app.review = Some(screen.with_local(local));
    app
}

/// Figma frame 92: the palette over the inbox with `re` typed. The entries are the engine's own, not the frame's sample.
fn palette_app() -> PrCraftApp {
    let mut app = inbox_app();
    app.palette = Some(crate::palette::Palette::typed("re"));
    app
}

fn review_app() -> PrCraftApp {
    let (screen, runs) = review_screen(false);
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, runs);
    app.notifications = Some(Notifications { events: Vec::new(), unseen: 3, errors: Vec::new(), pause: None, system_notice: "" });
    app.review = Some(screen);
    app
}

/// The look and fonts the app installs on a Mac without Geist: egui's stock faces, with the Mac's own symbol faces behind them.
/// Renders made with anything else could hide a missing glyph.
fn install_look(ctx: &Context, dark: bool) {
    use pr_craft_core::settings::{Settings, Theme};
    let look = Look::new(&Settings { theme: if dark { Theme::Dark } else { Theme::Light }, ..Settings::default() }, theme::System::default());
    theme::install(ctx, look);
    theme::apply_style(ctx, &look);
    crate::fonts::install_blocking(ctx, &pr_craft_core::settings::Settings::default());
}

fn window(app: &mut PrCraftApp, ui: &mut Ui, ready: &mut bool) {
    if !*ready {
        crate::prefs::install_defaults(ui.ctx());
        install_look(ui.ctx(), app.dark);
        *ready = true;
    }
    // The harness leaves a margin around the root; the window owns every pixel.
    let screen = ui.ctx().content_rect();
    let mut full = ui.new_child(egui::UiBuilder::new().max_rect(screen));
    full.set_clip_rect(screen);
    app.window(&mut full);
}

pub(crate) fn harness(app: PrCraftApp, width: f32, height: f32) -> Harness<'static, PrCraftApp> {
    let mut ready = false;
    Harness::builder().with_size(vec2(width, height)).with_pixels_per_point(1.0).build_ui_state(move |ui, app| window(app, ui, &mut ready), app)
}

/// Whether a node of `role` has a name that starts with `visible`, the label a sighted reader sees.
pub(crate) fn named(harness: &Harness<'_, PrCraftApp>, role: Role, visible: &str) -> bool {
    harness.query_all(By::new().role(role)).any(|node| node.accesskit_node().label().is_some_and(|name| name.starts_with(visible)))
}

#[test]
fn the_inbox_names_its_navigation_rows_and_actions_to_assistive_technology() {
    let harness = harness(inbox_app(), 1440.0, 900.0);
    let first = inbox_changes().remove(0);
    let forge = first.forge.name();
    let (open_on, stack) = (format!("Open on {forge}"), format!("Stack · {}", inbox_changes()[1].repo));
    for name in ["Review requested", "Back to you", "Notifications", "Open review", open_on.as_str(), first.title.as_str(), stack.as_str()] {
        assert!(named(&harness, Role::Button, name), "no button is named {name:?}");
    }
    for forge in Forge::ALL.map(Forge::name).into_iter().chain(["All"]) {
        assert!(named(&harness, Role::RadioButton, forge), "no filter option is named {forge:?}");
    }
}

#[test]
fn tab_moves_from_cancel_to_check_out_here_in_the_confirm_step() {
    let mut confirm = harness(local_app(LocalCase::Confirm), 1440.0, 900.0);
    confirm.run();
    let focused = |h: &Harness<'_, PrCraftApp>, name: &str| {
        h.query_all(By::new().role(Role::Button))
            .any(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with(name)) && n.accesskit_node().is_focused())
    };
    assert!(focused(&confirm, "Cancel"), "the safe choice has focus when the step opens");
    confirm.key_press(egui::Key::Tab);
    confirm.run();
    assert!(focused(&confirm, "Check out here"), "Tab reaches the confirming button instead of being pulled back to Cancel");
}

#[test]
fn open_locally_names_its_choices_and_a_refused_one_says_why() {
    let ready = harness(local_app(LocalCase::Ready), 1440.0, 900.0);
    for name in
        ["Open  ", "Check out in a worktree", "Check out here", "Open invoicer.go:", "Open in Xcode", "Open in Zed", "Open Terminal at the checkout"]
    {
        assert!(named(&ready, Role::Button, name), "no button is named {name:?}");
    }
    let confirm = harness(local_app(LocalCase::Confirm), 1440.0, 900.0);
    for name in ["Cancel", "Check out here"] {
        assert!(named(&confirm, Role::Button, name), "the confirm step has no button named {name:?}");
    }
    assert!(!named(&confirm, Role::Button, "Open in Zed"), "the other choices wait while the question is open");
    let dirty = harness(local_app(LocalCase::Dirty), 1440.0, 900.0);
    assert!(named(&dirty, Role::Button, "Check out here. The clone has uncommitted changes"), "the refusal is part of the choice's name");
    let setup = harness(local_app(LocalCase::NoClone), 1440.0, 900.0);
    assert!(named(&setup, Role::Button, "Check out in a worktree. No local clone"), "with no clone the worktree choice says to register one");
}

#[test]
fn an_svg_change_names_its_pictures_markup_rows_and_draft_to_assistive_technology() {
    let harness = harness(svg_app(), 1440.0, 900.0);
    for picture in ["Before, 412 B, 24 × 24 viewBox · shown at 60 px", "After, 538 B"] {
        assert!(named(&harness, Role::Image, picture), "no picture is named {picture:?}");
    }
    let rows = [
        "<path#arrow> d: M4 12a8 8 0 1 0 2.3-5.7 (31 points) changed to M3 11a9 9 0 0 1 15-5.3 (46 points)",
        "<path#arrow> stroke-width: 1.5 changed to 2",
        "<path#tail>: added: second arrow, 15 points",
        "<title>: added: \"Retry\"",
    ];
    for name in rows.iter().copied().chain(["Edit  E"]) {
        assert!(named(&harness, Role::Button, name), "no button is named {name:?}");
    }
    assert!(named(&harness, Role::RadioButton, "Structure, File view"));
}

#[test]
fn the_review_names_its_files_verdicts_and_actions_to_assistive_technology() {
    let harness = harness(review_app(), 1440.0, 900.0);
    let (screen_files, _) = (parse_unified(INVOICER_DIFF), ());
    let file = screen_files[0].path.rsplit('/').next().unwrap_or_default().to_string();
    for name in ["‹ Inbox", file.as_str(), "Submit review", "Keep as draft", "Edit first", "Discard", "AI review", "Share image"] {
        assert!(named(&harness, Role::Button, name), "no button is named {name:?}");
    }
    for verdict in Verdict::offered(Forge::GitHub) {
        assert!(named(&harness, Role::RadioButton, verdict.word()), "no verdict option is named {:?}", verdict.word());
    }
}

#[test]
fn both_screens_open_at_the_smallest_window_with_their_actions_inside_it() {
    for app in [inbox_app(), review_app(), local_app(LocalCase::Ready)] {
        let width = 1100.0;
        let harness = harness(app, width, 720.0);
        assert!(named(&harness, Role::Button, "Open review") || named(&harness, Role::Button, "Submit review"));
        // A control pushed past the right edge by a long row is a clipped control: every button ends inside the window.
        for node in harness.query_all(By::new().role(Role::Button)) {
            assert!(node.rect().right() <= width, "{:?} ends outside the window", node.accesskit_node().label());
        }
    }
}

/// Runs "Back to the inbox" from the command palette, by a click on its row or by ↵, with the typed query still holding focus.
fn back_by_palette(review: ReviewScreen, runs: Runs, click: bool) -> Harness<'static, PrCraftApp> {
    let changes = inbox_changes();
    let selected = changes[0].url.clone();
    let mut app = PrCraftApp::fixture(changes, &selected, runs);
    app.review = Some(review);
    app.palette = Some(crate::palette::Palette::typed("Back to the inbox"));
    let mut harness = harness(app, 1440.0, 900.0);
    harness.run();
    if click {
        harness.get_by_label_contains("Back to the inbox").click();
    } else {
        harness.key_press(egui::Key::Enter);
    }
    for _ in 0..6 {
        harness.step();
    }
    harness
}

#[test]
fn back_to_the_inbox_from_the_palette_leaves_the_review_whether_clicked_or_entered() {
    for click in [true, false] {
        let (screen, runs) = review_screen(false);
        let harness = back_by_palette(screen, runs, click);
        assert!(harness.state().palette.is_none(), "the palette closed (click: {click})");
        assert!(harness.state().review.is_none(), "the action reached the review as a key (click: {click})");
    }
}

#[test]
fn back_to_the_inbox_from_the_palette_leaves_even_with_a_line_selected() {
    for click in [true, false] {
        let (screen, runs) = review_screen(false);
        let harness = back_by_palette(screen.with_selected_line(), runs, click);
        assert!(harness.state().review.is_none(), "Back leaves, it does not first clear the selection (click: {click})");
    }
}

#[test]
fn back_to_the_inbox_from_the_palette_does_not_throw_away_an_open_comment() {
    for click in [true, false] {
        let (screen, runs) = review_screen(false);
        let harness = back_by_palette(screen.with_composer("keep this"), runs, click);
        let review = harness.state().review.as_ref().expect("the review stays while a comment is open");
        assert_eq!(review.composer_text(), Some("keep this"), "the comment is kept (click: {click})");
    }
}

#[test]
fn only_threads_that_are_drawn_can_be_stepped_to_or_resolved() {
    let (drawn, _) = thread_screen();
    let stops = drawn.thread_stops();
    assert!(stops > 0, "threads in an open file are stepped through");
    let (screen, _) = thread_screen();
    let folded = screen.with_viewed("internal/ledger/invoicer.go");
    assert_eq!(folded.thread_stops(), 0, "a viewed file is folded, so its threads are not drawn and ⇧R must not reach them");
}

/// Every control and drawn surface of each screen names itself (rule 21): egui_kittest fails a frame with an unnamed widget.
#[test]
fn every_screen_gives_all_its_widgets_an_accessible_name() {
    let screens: Vec<(&str, PrCraftApp)> = vec![
        ("inbox", inbox_app()),
        ("review", review_app()),
        ("back to you", back_app()),
        ("notifications", notifications_app()),
        ("since your review", since_app()),
        ("AI review sheet", sheet_app()),
        ("share sheet", share_app()),
        ("split view with diff options", split_app(true)),
        ("threads and expand bars", threads_app()),
        ("commit picker", commits_app()),
        ("command palette", palette_app()),
        ("new agent", agent_new_app()),
        ("activity", activity_app()),
        ("selected lines", lines_app()),
        ("files list with folders", tree_app()),
        ("search finds nothing", search_empty_app()),
        ("no drafts on this Mac", no_drafts_app()),
        ("every file viewed", all_viewed_app()),
        ("notifications, caught up", caught_up_app()),
        ("activity, nothing running", idle_activity_app()),
        ("local drafts", local_drafts_app(true)),
        ("no local drafts", local_drafts_app(false)),
        ("settings, AI engine", settings_app(Tab::Engine)),
        ("settings, appearance", settings_app(Tab::Appearance)),
        ("settings, keyboard", settings_app(Tab::Keyboard)),
    ];
    for (_, app) in screens {
        harness(app, 1440.0, 900.0).run();
    }
}

/// The names of the items of the open context menu.
fn menu_items(harness: &Harness<'_, PrCraftApp>) -> Vec<String> {
    let items = harness.query_all(By::new().role(Role::MenuItem)).chain(harness.query_all(By::new().role(Role::MenuItemCheckBox)));
    items.filter_map(|node| node.accesskit_node().label().map(|l| l.to_string())).collect()
}

/// Whether anything on screen has a name that contains `text`.
pub(crate) fn says(harness: &Harness<'_, PrCraftApp>, text: &str) -> bool {
    harness.query_all(By::new().label_contains(text)).next().is_some()
}

fn menu_open(harness: &Harness<'_, PrCraftApp>) -> bool {
    harness.query_all(By::new().role(Role::Menu)).next().is_some()
}

#[test]
fn a_change_has_a_menu_by_right_click_and_by_the_context_menu_key_and_every_item_names_its_key() {
    let mut harness = harness(inbox_app(), 1440.0, 900.0);
    harness.run_steps(5);
    harness.get_by_label_contains("Move settings page").click_secondary();
    harness.run_steps(5);
    assert!(menu_open(&harness), "a right-click opens the menu");
    let items = menu_items(&harness);
    for (name, key) in [("Open review", "↵"), ("Open on GitHub", "⌥↵"), ("Copy link", "⇧⌘C")] {
        assert!(items.iter().any(|i| i.starts_with(name) && i.ends_with(key)), "{name} {key} is in {items:?}");
    }
    harness.key_press(egui::Key::Escape);
    harness.run_steps(5);
    assert!(!menu_open(&harness), "Esc closes it");
    // The keyboard: focus the row, then ⇧F10.
    harness.get_by_label_contains("Pin Terraform").focus();
    harness.run_steps(5);
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F10);
    harness.run_steps(5);
    assert!(menu_open(&harness), "⇧F10 on a focused row opens its menu");
}

#[test]
fn choosing_copy_link_in_the_change_menu_does_what_its_key_does_and_closes_the_menu() {
    let mut harness = harness(inbox_app(), 1440.0, 900.0);
    harness.run_steps(5);
    harness.get_by_label_contains("Move settings page").click_secondary();
    harness.run_steps(5);
    harness.key_press(egui::Key::ArrowDown);
    harness.key_press(egui::Key::ArrowDown);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(5);
    assert!(!menu_open(&harness), "choosing closes the menu");
    assert_eq!(harness.state().runs.status, Some(Ok("Copied the link.".to_string())));
}

#[test]
fn the_menu_of_selected_lines_names_them_and_its_first_item_comments_on_them() {
    let mut harness = harness(lines_app(), 1440.0, 900.0);
    harness.run_steps(5);
    harness.get_by_label_contains("43 +").click_secondary();
    harness.run_steps(5);
    assert!(harness.query_by_label_contains("Context menu, Lines 42–44 · invoicer.go").is_some(), "the menu names the lines it belongs to");
    let items = menu_items(&harness);
    for name in ["Comment on these lines  C", "Suggest a change, unavailable: Not built yet", "Hide whitespace changes  W", "Mark file viewed  V"] {
        assert!(items.iter().any(|i| i == name), "{name} is in {items:?}");
    }
    harness.key_press(egui::Key::Enter);
    harness.run_steps(5);
    assert_eq!(harness.state().review.as_ref().and_then(|r| r.composer_text()), Some(""), "↵ chose Comment, which opened the comment box");
}

/// What the review under the app says about itself: file, selected lines, Open locally popover, and the marks a folder would change.
fn review_probe(harness: &Harness<'_, PrCraftApp>, dir: &str) -> crate::review::Probe {
    harness.state().review.as_ref().map(|r| r.probe_more(dir)).unwrap_or_default()
}

fn click_item(harness: &mut Harness<'_, PrCraftApp>, name: &str) {
    harness.get_by_label_contains(name).click();
    harness.run_steps(5);
}

#[test]
fn a_folder_menu_folds_the_folder_and_narrows_the_filter_with_the_same_effect_as_its_keys() {
    let mut harness = harness(tree_app(), 1440.0, 900.0);
    harness.run_steps(5);
    let folded = |h: &Harness<'_, PrCraftApp>| h.state().review.as_ref().map(|r| r.probe().1).unwrap_or_default();
    harness.get_by_label_contains("internal/webhooks, ").click_secondary();
    harness.run_steps(5);
    assert!(says(&harness, "Context menu, internal/webhooks · 3 files"));
    click_item(&mut harness, "Collapse folder");
    assert_eq!(folded(&harness), vec!["internal/webhooks".to_string()], "Collapse folder");
    assert!(!says(&harness, "worker.go, "), "the folded folder's files leave the list");
    harness.get_by_label_contains("internal/webhooks, ").click_secondary();
    harness.run_steps(5);
    click_item(&mut harness, "Open folder");
    assert!(folded(&harness).is_empty() && says(&harness, "worker.go, "), "Open folder brings them back");
    // Show only: the files of the folder stay and the others leave.
    harness.get_by_label_contains("internal/webhooks, ").click_secondary();
    harness.run_steps(5);
    click_item(&mut harness, "Show only this folder");
    assert!(says(&harness, "worker.go, ") && !says(&harness, "webhooks.md, "), "the filter keeps the folder's files and hides the rest");
}

#[test]
fn the_folder_keys_and_the_folder_menu_agree_on_which_folder_they_act_on() {
    let mut harness = harness(tree_app(), 1440.0, 900.0);
    harness.run_steps(5);
    let folded = |h: &Harness<'_, PrCraftApp>| h.state().review.as_ref().map(|r| r.probe().1).unwrap_or_default();
    // The current file is in internal/webhooks: the menu of another folder shows no key it would not obey.
    harness.get_by_label_contains("db/migrations, ").click_secondary();
    harness.run_steps(5);
    assert!(
        menu_items(&harness).iter().any(|i| i == "Mark folder unviewed"),
        "no key on a folder the keys do not act on: {:?}",
        menu_items(&harness)
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(5);
    harness.get_by_label_contains("internal/webhooks, ").click_secondary();
    harness.run_steps(5);
    assert!(menu_items(&harness).iter().any(|i| i.starts_with("Mark folder viewed  ⇧V")), "{:?}", menu_items(&harness));
    harness.key_press(egui::Key::Escape);
    harness.run_steps(5);
    // ← folds the current file's folder, → opens it, ⌥← folds every folder.
    harness.key_press(egui::Key::ArrowLeft);
    harness.run_steps(5);
    assert_eq!(folded(&harness), vec!["internal/webhooks".to_string()]);
    harness.key_press(egui::Key::ArrowRight);
    harness.run_steps(5);
    assert!(folded(&harness).is_empty());
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowLeft);
    harness.run_steps(5);
    assert!(folded(&harness).len() > 1 && folded(&harness).contains(&"internal/webhooks".to_string()), "{:?}", folded(&harness));
    // The marks a folder would change: viewed for a folder with an unviewed file, unviewed when all are viewed.
    let (want, changing) = review_probe(&harness, "internal/webhooks").3;
    assert!(want && !changing.is_empty() && changing.iter().all(|p| p.starts_with("internal/webhooks/")));
    let (want, changing) = review_probe(&harness, "db/migrations").3;
    assert!(!want && !changing.is_empty(), "every file of it is viewed, so marking undoes them");
}

#[test]
fn the_arrow_keys_edit_the_files_filter_and_never_fold_a_folder_while_it_has_focus() {
    let mut harness = harness(tree_app(), 1440.0, 900.0);
    harness.run_steps(5);
    harness.query_all(By::new().role(Role::TextInput)).next().expect("the Files filter").focus();
    harness.run_steps(5);
    harness.key_press(egui::Key::ArrowLeft);
    harness.run_steps(5);
    assert!(harness.state().review.as_ref().map(|r| r.probe().1).unwrap_or_default().is_empty());
}

#[test]
fn a_folder_the_filter_cannot_write_has_its_filter_items_unavailable_and_choosing_one_changes_nothing() {
    let mut harness = harness(spaced_folder_app(), 1440.0, 900.0);
    harness.run_steps(5);
    harness.get_by_label_contains("docs/My Notes, ").click_secondary();
    harness.run_steps(5);
    let items = menu_items(&harness);
    for name in ["Show only this folder", "Hide this folder"] {
        let item = items.iter().find(|i| i.starts_with(name)).unwrap_or_else(|| panic!("{name} is in {items:?}"));
        assert!(item.contains("unavailable: The folder name has characters the filter cannot hold"), "{item}");
    }
    let before = harness.state().review.as_ref().map(|r| r.probe().2).unwrap_or_default();
    click_item(&mut harness, "Show only this folder");
    assert_eq!(harness.state().review.as_ref().map(|r| r.probe().2).unwrap_or_default(), before, "an unavailable item does nothing");
    assert!(menu_open(&harness), "and the menu stays open for another choice");
}

#[test]
fn the_lines_menu_acts_on_the_file_it_names_even_when_another_file_is_current() {
    let mut harness = harness(lines_app_on_other_file(), 1440.0, 900.0);
    harness.run_steps(5);
    assert_eq!(review_probe(&harness, "").0, 1, "another file is current");
    harness.get_by_label_contains("43 +").click_secondary();
    harness.run_steps(5);
    assert!(says(&harness, "Context menu, Lines 42–44 · invoicer.go"));
    assert_eq!(review_probe(&harness, "").0, 0, "the menu's items act on the file of the selection it names");
}

#[test]
fn the_editor_item_opens_the_popover_like_its_key_when_the_popover_is_closed() {
    let mut harness = harness(lines_app(), 1440.0, 900.0);
    harness.run_steps(5);
    harness.get_by_label_contains("43 +").click_secondary();
    harness.run_steps(5);
    assert!(menu_items(&harness).iter().any(|i| i.starts_with("Open locally…  ")), "{:?}", menu_items(&harness));
    assert!(!review_probe(&harness, "").2);
    click_item(&mut harness, "Open locally…");
    assert!(review_probe(&harness, "").2, "the item does what ⌘O does: it opens the popover instead of failing to find a clone");
}

fn split_pressed(harness: &mut Harness<'_, PrCraftApp>, side: &str) -> Option<(String, usize)> {
    let name = if side == "old" { "if n > 3 {, old side" } else { "if n >= r.cfg.MaxAttempts {, new side" };
    harness.get_by_label_contains(name).click();
    harness.run_steps(5);
    review_probe(harness, "").1
}

#[test]
fn a_press_on_a_half_of_a_split_row_selects_that_halfs_line_and_its_menu_names_it() {
    let mut harness = harness(split_app(false), 1440.0, 900.0);
    harness.run_steps(5);
    let old = split_pressed(&mut harness, "old").expect("a line is selected");
    assert!(old.0.contains("if n > 3 {") && old.1 == 1, "{old:?}");
    let new = split_pressed(&mut harness, "new").expect("a line is selected");
    assert!(new.0.contains("MaxAttempts") && new.1 == 1, "{new:?}");
    harness.get_by_label_contains("if n > 3 {, old side").click_secondary();
    harness.run_steps(5);
    assert!(says(&harness, "Context menu, Line 32 · retry.go"), "the removed line is anchored by its old number");
    assert_eq!(review_probe(&harness, "").1.map(|(text, _)| text.contains("if n > 3 {")), Some(true), "right-click selected the pressed half");
}

#[test]
fn back_to_you_does_not_say_nobody_is_waiting_over_a_re_check_that_failed() {
    let mut empty = back_app();
    for back in &mut empty.back {
        back.sync = BackFetch::Done(BackState::Listed { items: Vec::new(), truncated: false, incomplete: None, yours: Vec::new() });
    }
    assert!(says(&harness(empty, 1440.0, 900.0), "Nobody is waiting on you"));
    let mut failed = back_app();
    for back in &mut failed.back {
        back.sync = BackFetch::Done(BackState::Listed { items: Vec::new(), truncated: false, incomplete: None, yours: Vec::new() });
        back.stale = Some("offline".into());
    }
    let failed = harness(failed, 1440.0, 900.0);
    assert!(!says(&failed, "Nobody is waiting on you"));
    assert!(says(&failed, "may be out of date"));
}

#[test]
fn focus_rings_follow_the_keyboard_and_not_the_pointer() {
    let mut harness = harness(inbox_app(), 1440.0, 900.0);
    harness.run_steps(5);
    harness.get_by_label_contains("Pin Terraform").click();
    harness.run_steps(5);
    assert!(!theme::ring_visible(&harness.ctx), "a click moves focus but shows no ring");
    harness.key_press(egui::Key::Tab);
    harness.run_steps(5);
    assert!(theme::ring_visible(&harness.ctx), "a key press does");
}

#[test]
fn an_empty_list_never_says_all_clear_over_data_that_is_missing() {
    // Search: the words say what was searched, and what a paused source left out.
    let found = harness(search_empty_app(), 1440.0, 900.0);
    assert!(says(&found, "Nothing matches “jitterbug”"));
    assert!(!says(&found, "paused or failed"));
    let mut paused = search_empty_app();
    paused.sources[0].sync =
        crate::Sync::Done(pr_craft_engine::SourceState::Paused { login: pr_craft_forge::Login::LoggedOut { fix: "gh auth login".into() } });
    let paused = harness(paused, 1440.0, 900.0);
    assert!(says(&paused, "A source is paused or failed"));
    // Notifications: "caught up" only when every source answered.
    assert!(says(&harness(caught_up_app(), 1440.0, 900.0), "You are caught up"));
    let mut broken = caught_up_app();
    broken.sources[1].sync = crate::Sync::Done(pr_craft_engine::SourceState::Failed { error: "gl: offline".into() });
    let broken = harness(broken, 1440.0, 900.0);
    assert!(!says(&broken, "You are caught up"));
    assert!(says(&broken, "Nothing to show yet"));
    // Activity: "Nothing is running" only when every run record was read.
    let idle = harness(idle_activity_app(), 1440.0, 900.0);
    assert!(says(&idle, "Nothing is running"));
    assert!(says(&idle, "Today: 6 runs finished, none failed."));
    let mut unread = idle_activity_app();
    unread.runs.error = Some("the runs folder is not readable".into());
    let unread = harness(unread, 1440.0, 900.0);
    assert!(!says(&unread, "Nothing is running"));
    assert!(says(&unread, "The runs could not be read"));
}

#[test]
fn the_local_drafts_queue_lists_what_is_saved_and_says_none_only_when_every_file_was_read() {
    let listed = harness(local_drafts_app(true), 1440.0, 900.0);
    assert!(says(&listed, "Local drafts  3"), "the sidebar row carries the count");
    for name in ["Retry idempotent webhooks with exponential backoff", "acme/ios #731"] {
        assert!(says(&listed, name), "{name} is listed");
    }
    assert!(says(&listed, "3 drafts · verdict: Request changes"), "the verdict is part of the row");
    let none = harness(local_drafts_app(false), 1440.0, 900.0);
    assert!(says(&none, "No drafts on this Mac"));
    assert!(named(&none, Role::Button, "Go to Review requested"), "one next action");
    let mut unread = local_drafts_app(false);
    crate::drafts::fixture_drafts(&mut unread, Vec::new(), vec!["reviews/github/acme%2Fapi/3.json: not json".into()]);
    let unread = harness(unread, 1440.0, 900.0);
    assert!(!says(&unread, "No drafts on this Mac"), "a file that could not be read may hold drafts");
    assert!(says(&unread, "could not be read"));
}

#[test]
fn the_review_says_when_every_file_is_viewed_and_when_a_change_has_no_drafts() {
    let mut done = harness(all_viewed_app(), 1440.0, 900.0);
    done.run_steps(5);
    assert!(says(&done, "All 4 files viewed"));
    assert!(named(&done, Role::Button, "Go to Submit"));
    done.get_by_label_contains("Show viewed files").click();
    done.run_steps(5);
    assert!(!says(&done, "All 4 files viewed"), "Show viewed files brings the list back");
    assert!(!says(&harness(review_app(), 1440.0, 900.0), "All 4 files viewed"), "unviewed files are not all viewed");
    assert!(says(&harness(no_drafts_app(), 1440.0, 900.0), "No drafts on this Mac"));
    assert!(!says(&harness(review_app(), 1440.0, 900.0), "No drafts on this Mac"));
}

#[cfg(feature = "renders")]
mod renders {
    use std::path::PathBuf;

    use craft::image_codecs::{ChannelLayout, EncodeOptions, Format, Image};

    use super::*;

    /// `app` in the dark theme: the look the window installs follows the flag the real window sets from the saved settings.
    fn dark(mut app: PrCraftApp) -> PrCraftApp {
        app.dark = true;
        app
    }

    fn out_dir() -> PathBuf {
        std::env::var_os("PR_CRAFT_RENDERS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.egm/parity/app")))
    }

    fn render(app: PrCraftApp, width: f32, height: f32, name: &str) {
        render_after(app, width, height, name, 0);
    }

    /// `render`, after `waits` more frames 30 ms apart, for a screen that draws on a thread (the share image).
    fn render_after(app: PrCraftApp, width: f32, height: f32, name: &str, waits: u32) {
        render_with(app, width, height, name, |harness| {
            for _ in 0..waits {
                std::thread::sleep(std::time::Duration::from_millis(30));
                harness.step();
            }
        });
    }

    /// `render`, after `prepare` has driven the harness: a hover, a key, a right-click.
    fn render_with(app: PrCraftApp, width: f32, height: f32, name: &str, prepare: impl FnOnce(&mut Harness<'static, PrCraftApp>)) {
        let mut ready = false;
        let mut harness = Harness::builder()
            .with_size(vec2(width, height))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_ui_state(move |ui, app| window(app, ui, &mut ready), app);
        harness.run();
        prepare(&mut harness);
        let image = harness.render().expect("wgpu renders the frame");
        let (w, h) = (image.width(), image.height());
        let pixels = Image::from_u8(w, h, ChannelLayout::Rgba, image.as_raw().to_vec()).expect("the pixels make an image");
        let png = craft::image_codecs::encode(&pixels, Format::Png, &EncodeOptions::default()).expect("PNG encodes");
        let dir = out_dir();
        std::fs::create_dir_all(&dir).expect("the render directory can be made");
        std::fs::write(dir.join(name), png).expect("the PNG can be written");
    }

    #[test]
    fn the_threads_the_commit_picker_and_the_palette_render_at_the_size_of_the_figma_frames() {
        render(threads_app(), 1440.0, 900.0, "90-threads.png");
        render(commits_app(), 1440.0, 900.0, "95-commits.png");
        render(palette_app(), 1440.0, 900.0, "92-palette.png");
    }
    #[test]
    fn open_locally_renders_at_the_size_of_the_figma_frame_and_in_its_other_states() {
        render(local_app(LocalCase::Ready), 1440.0, 900.0, "96-open-locally.png");
        render(local_app(LocalCase::Confirm), 1440.0, 900.0, "96-open-locally-confirm.png");
        render(local_app(LocalCase::NoClone), 1440.0, 900.0, "96-open-locally-setup.png");
        render(local_app(LocalCase::Dirty), 1440.0, 900.0, "96-open-locally-dirty.png");
        render(local_app(LocalCase::Ready), 1100.0, 720.0, "96-open-locally-small.png");
    }

    #[test]
    fn the_split_view_renders_at_the_size_of_the_figma_frame() {
        render(split_app(true), 1440.0, 900.0, "91-split.png");
    }

    #[test]
    fn the_svg_review_renders_at_the_size_of_the_figma_frame() {
        render(svg_app(), 1440.0, 900.0, "79-svg.png");
    }

    #[test]
    fn the_inbox_and_the_review_render_at_the_size_of_the_figma_frames() {
        render(inbox_app(), 1440.0, 900.0, "01-inbox.png");
        render(review_app(), 1440.0, 900.0, "02-review.png");
    }

    #[test]
    fn the_back_to_you_and_notifications_lists_render_at_the_size_of_the_figma_frames() {
        render(back_app(), 1440.0, 900.0, "21-back-to-you.png");
        render(notifications_app(), 1440.0, 900.0, "07-notifications.png");
    }

    #[test]
    fn the_share_sheet_renders_at_the_size_of_the_figma_frame() {
        render_after(share_app(), 1440.0, 900.0, "19-share.png", 100);
    }

    #[test]
    fn the_activity_screen_renders_at_the_size_of_the_figma_frame() {
        render(activity_app(), 1440.0, 900.0, "61-activity.png");
    }

    #[test]
    fn the_new_agent_editor_renders_at_the_size_of_the_figma_frame() {
        render(agent_new_app(), 1440.0, 900.0, "12-agent-new.png");
    }

    #[test]
    fn the_ai_review_sheet_renders_at_the_size_of_the_figma_frame() {
        render(sheet_app(), 1440.0, 900.0, "29-ai-sheet.png");
    }

    #[test]
    fn the_since_your_review_screen_renders_at_the_size_of_the_figma_frame() {
        render(since_app(), 1440.0, 900.0, "22-since-review.png");
    }

    #[test]
    fn the_settings_pages_render_at_the_size_of_the_figma_frames() {
        for (tab, name) in
            [(Tab::Appearance, "17-settings-appearance.png"), (Tab::Engine, "58-settings-ai.png"), (Tab::Keyboard, "59-settings-keyboard.png")]
        {
            render(settings_app(tab), 1440.0, 900.0, name);
        }
    }

    #[test]
    fn appearance_renders_at_the_narrowest_wide_layout() {
        render(settings_app(Tab::Appearance), 1280.0, 800.0, "17-settings-appearance-1280.png");
    }

    #[test]
    fn the_other_screens_render_at_the_smallest_window() {
        let screens: Vec<(&str, PrCraftApp, u32)> = vec![
            ("07-notifications", notifications_app(), 0),
            ("12-agent-new", agent_new_app(), 0),
            ("17-settings-appearance", settings_app(Tab::Appearance), 0),
            ("19-share", share_app(), 100),
            ("21-back-to-you", back_app(), 0),
            ("22-since-review", since_app(), 0),
            ("29-ai-sheet", sheet_app(), 0),
            ("58-settings-ai", settings_app(Tab::Engine), 0),
            ("59-settings-keyboard", settings_app(Tab::Keyboard), 0),
            ("61-activity", activity_app(), 0),
        ];
        for (name, app, waits) in screens {
            render_after(app, 1100.0, 720.0, &format!("{name}-small.png"), waits);
        }
    }

    #[test]
    fn both_screens_render_at_the_smallest_window() {
        render(inbox_app(), 1100.0, 720.0, "01-inbox-1100x720.png");
        render(review_app(), 1100.0, 720.0, "02-review-1100x720.png");
    }

    /// Hovers the button whose name starts with `name`, and lets the frame settle.
    fn hover(harness: &mut Harness<'static, PrCraftApp>, name: &str) {
        let row = harness
            .query_all(By::new().role(Role::Button))
            .find(|node| node.accesskit_node().label().is_some_and(|label| label.starts_with(name)))
            .expect("a button of that name");
        row.hover();
        harness.run();
    }

    #[test]
    fn the_interaction_states_render_at_the_size_of_the_figma_frames() {
        // 102: a row under the pointer and another with keyboard focus (J J puts it on the third change).
        render_with(inbox_app(), 1440.0, 900.0, "102-states.png", |harness| {
            harness.key_press(egui::Key::J);
            harness.key_press(egui::Key::J);
            harness.run();
            hover(harness, "Pin Terraform");
        });
        // 103: a right-click inside the selection, then the pointer over another line for the comment button.
        render_with(lines_app(), 1440.0, 900.0, "103-states.png", |harness| {
            harness.get_by_label_contains("43 +").click_secondary();
            harness.run();
            hover(harness, "37 ");
        });
        // 104: a right-click on a change.
        render_with(inbox_app(), 1440.0, 900.0, "104-states.png", |harness| {
            harness.get_by_label_contains("Move settings page").click_secondary();
            harness.run();
        });
        // 105: a right-click on a folder of the Files list.
        render_with(tree_app(), 1440.0, 900.0, "105-states.png", |harness| {
            harness.get_by_label_contains("internal/webhooks, ").click_secondary();
            harness.run();
        });
        render(search_empty_app(), 1440.0, 900.0, "106-states.png");
        render(local_drafts_app(false), 1440.0, 900.0, "107-states.png");
        render(local_drafts_app(true), 1440.0, 900.0, "01-local-drafts.png");
        render(no_drafts_app(), 1440.0, 900.0, "107-review-states.png");
        // The comment button of a hovered line in split view.
        render_with(split_app(false), 1440.0, 900.0, "103-states-split.png", |harness| hover(harness, "32 +"));
        render(all_viewed_app(), 1440.0, 900.0, "108-states.png");
        render(caught_up_app(), 1440.0, 900.0, "109-states.png");
        render(idle_activity_app(), 1440.0, 900.0, "110-states.png");
    }

    /// Figma frames 27, 75, 97, 94, 88 and 89 (Your changes), the two confirm steps, and the narrowest window.
    #[test]
    fn your_changes_renders_at_the_size_of_the_figma_frames() {
        use crate::mine_parity as mine;
        render(mine::failing_app(), 1440.0, 900.0, "27-m7.png");
        render(mine::author_app(), 1440.0, 900.0, "75-m7.png");
        render(mine::draft_app(), 1440.0, 900.0, "97-m7.png");
        render(mine::checks_app(false), 1440.0, 900.0, "94-m7.png");
        render(mine::merge_app(None), 1440.0, 900.0, "88-m7.png");
        render(mine::conflict_app(), 1440.0, 900.0, "89-m7.png");
        render(mine::merge_app(Some("merge")), 1440.0, 900.0, "88-m7-confirm.png");
        render(mine::checks_app(true), 1440.0, 900.0, "94-m7-rerun.png");
        render(mine::author_app(), 1100.0, 720.0, "75-m7-small.png");
    }

    /// Figma frames 114 to 118, the dark theme: the split view with Diff options, the palette, a right-click on a change, and
    /// Activity with nothing running. Frames 113 (Your changes, Merge) and 116 (Checks) are not built, so they have no render.
    #[test]
    fn the_dark_screens_render_at_the_size_of_the_figma_frames() {
        render(dark(split_app(true)), 1440.0, 900.0, "114-dark.png");
        render(dark(palette_app()), 1440.0, 900.0, "115-dark.png");
        render_with(dark(inbox_app()), 1440.0, 900.0, "117-dark.png", |harness| {
            harness.get_by_label_contains("Move settings page").click_secondary();
            harness.run();
        });
        render(dark(idle_activity_app()), 1440.0, 900.0, "118-dark.png");
    }
}
