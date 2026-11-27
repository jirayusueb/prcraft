//! The reading pane of one of the reader's own changes (Figma frames 27, 75, 97, 88, 89): the head line, the checks banner, and the
//! card for the state the change is in (draft, conflict, ready or not to merge), the reviewers' threads, and the actions pinned at
//! the foot. Every write is an engine command run off the UI thread; a merge, a queue or Merge when ready asks first, naming the
//! method and the branch. Babysit and the AI conflict fix are not built, so the pane says so and draws no control for them.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use craft::egui::{self, Align, Color32, Context, Layout, Margin, OpenUrl, Rect, RichText, ScrollArea, Stroke, TextEdit, Ui, UiBuilder, vec2};
use pr_craft_core::mine::{CheckState, Decision, MergeMethod, MergeState, Mine, MineDetail};
use pr_craft_core::thread::ReviewThread;
use pr_craft_core::{ChangeRef, Forge};
use pr_craft_engine::mine::{self as engine, Merge, MergeAsk};
use pr_craft_engine::thread::ThreadList;
use pr_craft_forge::act::{Label, Page, Person};

use crate::components::{self, BannerKind, Pill, PillKind, TagKind};
use crate::{age, keymap, live_label, theme};

/// How long the confirm step is shown before it can be answered.
const ARM: Duration = Duration::from_millis(600);

/// A read or a write on its way off the UI thread.
pub(crate) enum Load<T> {
    Idle,
    Pending(Receiver<T>),
    Ready(T),
}

impl<T: Send + 'static> Load<T> {
    pub(crate) fn start(ctx: &Context, work: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // The window may have closed first; nobody is left to tell.
            let _ = tx.send(work());
            ctx.request_repaint();
        });
        Load::Pending(rx)
    }
}

impl<T> Load<Result<T, String>> {
    /// Takes the answer when it has arrived; true on the frame it does.
    pub(crate) fn poll(&mut self) -> bool {
        let Load::Pending(rx) = self else { return false };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the read stopped before answering".to_string()),
        };
        *self = Load::Ready(answer);
        true
    }

    pub(crate) fn busy(&self) -> bool {
        matches!(self, Load::Pending(_))
    }

    pub(crate) fn ready(&self) -> Option<&Result<T, String>> {
        match self {
            Load::Ready(answer) => Some(answer),
            _ => None,
        }
    }

    pub(crate) fn ok(&self) -> Option<&T> {
        self.ready().and_then(|r| r.as_ref().ok())
    }

    fn take(&mut self) -> Option<Result<T, String>> {
        match std::mem::replace(self, Load::Idle) {
            Load::Ready(answer) => Some(answer),
            other => {
                *self = other;
                None
            }
        }
    }
}

/// The reviewers and labels the reader has picked for a draft, kept until a button sends them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Picks {
    reviewers: Vec<String>,
    labels: Vec<String>,
}

struct Form {
    method: Option<MergeMethod>,
    message: String,
    /// The reader changed the message, so a new method does not replace it.
    edited: bool,
    delete_branch: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Merge,
    Queue,
    Auto,
}

/// What the primary button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Carry out the step the reader is being asked to confirm.
    Confirm,
    Ready,
    Conflict,
    Ask,
    /// Nothing can be merged yet: the reader is sent to the review.
    Review,
    Merge,
    Queue,
}
/// What the pane asks the app to do.
#[derive(Default)]
pub(crate) struct Out {
    pub review: bool,
    pub checks: bool,
    /// The check the log should open on.
    pub check: Option<String>,
}

pub(crate) struct Panel {
    key: String,
    change: ChangeRef,
    live: bool,
    detail: Load<Result<MineDetail, String>>,
    threads: Load<Result<ThreadList, String>>,
    people: Load<Result<Page<Person>, String>>,
    labels: Load<Result<Page<Label>, String>>,
    picks: Option<Picks>,
    /// What the forge said when the picks were last seeded: only the reader's edit of it is ever sent.
    seed: Picks,
    form: Form,
    confirm: Option<Confirm>,
    /// When the confirm step opened, and the commit the reader saw then: the merge is pinned to it.
    confirm_since: Option<Instant>,
    confirm_head: String,
    focus_confirm: bool,
    focus_primary: bool,
    /// The row of the list this pane was last read for: a change in either means the pane is out of date.
    seen_at: DateTime<Utc>,
    seen_head: String,
    /// How many writes were started, for the tests.
    sends: u32,
    /// A write on its way; its answer is the message of the status strip.
    acting: Load<Result<String, String>>,
}

fn same_set(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| y.eq_ignore_ascii_case(x)))
}

/// Sets the reviewers and labels that differ, then makes the draft ready when asked. The first step that fails stops the rest, and
/// the answer says which ones went through.
fn ready_work(change: &ChangeRef, seed: &Picks, picks: &Picks, ready: bool) -> Result<String, String> {
    let mut done: Vec<String> = Vec::new();
    let stop = |done: &[String], e: String| if done.is_empty() { e } else { format!("{}. Then it stopped: {e}", done.join(", ")) };
    // Only what the reader changed is sent, and it is applied to who is asked and what is labelled now: a reviewer or a label added
    // since the pane was read stays.
    let (add, remove) = delta(&seed.reviewers, &picks.reviewers);
    if !add.is_empty() || !remove.is_empty() {
        engine::pr_change_reviewers(change, &add, &remove).map_err(|e| stop(&done, e.to_string()))?;
        done.push("Reviewers set".into());
    }
    let (add, remove) = delta(&seed.labels, &picks.labels);
    if !add.is_empty() || !remove.is_empty() {
        engine::pr_change_labels(change, &add, &remove).map_err(|e| stop(&done, e.to_string()))?;
        done.push("Labels set".into());
    }
    if ready {
        done.push(engine::pr_ready(change).map_err(|e| stop(&done, e.to_string()))?.message);
    }
    Ok(if done.is_empty() { "Nothing to change.".to_string() } else { format!("{}.", done.join(". ").trim_end_matches('.')) })
}

/// What `picks` adds to and removes from `seed` (case aside): the reader's own edit.
fn delta(seed: &[String], picks: &[String]) -> (Vec<String>, Vec<String>) {
    let has = |list: &[String], who: &String| list.iter().any(|x| x.eq_ignore_ascii_case(who));
    (picks.iter().filter(|w| !has(seed, w)).cloned().collect(), seed.iter().filter(|w| !has(picks, w)).cloned().collect())
}

/// The reader's edit of `old_seed` (`picks`) made on top of what the forge says now (`fresh`).
fn rebased(old_seed: &[String], picks: &[String], fresh: &[String]) -> Vec<String> {
    let (add, remove) = delta(old_seed, picks);
    let mut now: Vec<String> = fresh.iter().filter(|w| !remove.iter().any(|r| r.eq_ignore_ascii_case(w))).cloned().collect();
    for who in add {
        if !now.iter().any(|w| w.eq_ignore_ascii_case(&who)) {
            now.push(who);
        }
    }
    now
}

/// Whether this frame carries a repeat of a held key: a held ↵ or ⌘↵ is not a second answer.
pub(crate) fn repeating(ui: &Ui) -> bool {
    ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { repeat: true, .. })))
}

fn chip(ui: &mut Ui, fill: Color32, ink: Color32, text: &str) {
    egui::Frame::new().fill(fill).corner_radius(10).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).size(11.5).color(ink)).wrap_mode(egui::TextWrapMode::Extend));
    });
}

impl Panel {
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// Opens the pane of `mine` and starts reading what the list does not carry. A fixture (`live` false) reads nothing.
    pub(crate) fn open(ctx: &Context, mine: &Mine, live: bool) -> Self {
        let change = mine.change.change_ref();
        let mut panel = Panel {
            key: mine.change.url.clone(),
            change,
            live,
            detail: Load::Idle,
            threads: Load::Idle,
            people: Load::Idle,
            labels: Load::Idle,
            picks: Some(Picks { reviewers: mine.requested.clone(), labels: mine.labels.clone() }),
            seed: Picks { reviewers: mine.requested.clone(), labels: mine.labels.clone() },
            form: Form { method: None, message: String::new(), edited: false, delete_branch: true },
            confirm: None,
            confirm_since: None,
            confirm_head: String::new(),
            focus_confirm: false,
            focus_primary: false,
            seen_at: mine.change.updated_at,
            seen_head: mine.head_sha.clone(),
            sends: 0,
            acting: Load::Idle,
        };
        panel.reload(ctx);
        panel
    }

    /// Reads the change and its threads again.
    fn reload(&mut self, ctx: &Context) {
        if !self.live {
            return;
        }
        let (a, b) = (self.change.clone(), self.change.clone());
        self.detail = Load::start(ctx, move || engine::detail(&a).map_err(|e| e.to_string()));
        self.threads = Load::start(ctx, move || pr_craft_engine::thread::list(&b).map_err(|e| e.to_string()));
    }

    /// A pane whose reads have all answered, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(mine: &Mine, detail: MineDetail, threads: Option<ThreadList>, people: Vec<Person>, labels: Vec<Label>) -> Self {
        let mut panel = Panel::open(&Context::default(), mine, false);
        panel.detail = Load::Ready(Ok(detail));
        panel.threads = threads.map_or(Load::Idle, |t| Load::Ready(Ok(t)));
        panel.people = Load::Ready(Ok(Page { items: people, truncated: false }));
        panel.labels = Load::Ready(Ok(Page { items: labels, truncated: false }));
        panel.seed();
        panel
    }

    #[cfg(test)]
    pub(crate) fn pick_for_test(&mut self, reviewers: &[&str], labels: &[&str]) {
        self.picks = Some(Picks {
            reviewers: reviewers.iter().map(|s| (*s).to_string()).collect(),
            labels: labels.iter().map(|s| (*s).to_string()).collect(),
        });
    }
    #[cfg(test)]
    pub(crate) fn confirming(&mut self, which: &str) {
        let which = match which {
            "queue" => Confirm::Queue,
            "auto" => Confirm::Auto,
            _ => Confirm::Merge,
        };
        self.open_confirm(which, &self.seen_head.clone());
        self.age_confirm();
    }

    /// Lets the confirm step's arming time pass.
    #[cfg(test)]
    pub(crate) fn age_confirm(&mut self) {
        self.confirm_since = Instant::now().checked_sub(ARM * 2);
    }

    /// The primary action once, as the button or its key would run it.
    #[cfg(test)]
    pub(crate) fn press(&mut self, ctx: &Context, m: &Mine) {
        self.primary(ctx, m, &mut Out::default());
    }

    #[cfg(test)]
    pub(crate) fn sends(&self) -> u32 {
        self.sends
    }

    #[cfg(test)]
    pub(crate) fn is_confirming(&self) -> bool {
        self.confirm.is_some()
    }

    /// Takes what the forge says now as the seed of the picks, keeping the reader's own edit on top of it.
    fn reseed(&mut self) {
        let Some(fresh) = self.detail.ok().map(|d| Picks { reviewers: d.mine.requested.clone(), labels: d.mine.labels.clone() }) else { return };
        if let Some(picks) = &mut self.picks {
            *picks = Picks {
                reviewers: rebased(&self.seed.reviewers, &picks.reviewers, &fresh.reviewers),
                labels: rebased(&self.seed.labels, &picks.labels, &fresh.labels),
            };
        }
        self.seed = fresh;
    }

    fn seed(&mut self) {
        let Some(detail) = self.detail.ok() else { return };
        if self.form.method.is_none() {
            self.form.method = [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase].into_iter().find(|m| detail.methods.contains(m));
        }
        if !self.form.edited {
            self.form.message = detail.message.clone();
        }
    }

    /// Takes the answers that have arrived. A write's answer comes back for the status strip; the change is read again after it,
    /// whether it worked or ran out of time, so what shows is what the forge says now.
    pub(crate) fn poll(&mut self, ctx: &Context) -> Option<Result<String, String>> {
        self.threads.poll();
        self.people.poll();
        self.labels.poll();
        if self.detail.poll() {
            self.reseed();
            self.seed();
        }
        if self.acting.poll() {
            self.confirm = None;
            self.confirm_since = None;
            // What the write changed must show: the held lists of Your changes are thrown away before anything reads them again.
            pr_craft_engine::mine::invalidate();
            self.reload(ctx);
            return self.acting.take();
        }
        None
    }

    fn current<'a>(&'a self, listed: &'a Mine) -> &'a Mine {
        self.detail.ok().map_or(listed, |d| &d.mine)
    }

    fn run(&mut self, ctx: &Context, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if self.acting.busy() {
            return;
        }
        self.sends += 1;
        // A fixture sends nothing: it counts what would have gone.
        if self.live {
            self.acting = Load::start(ctx, work);
        }
    }

    /// Backs out of the confirm step.
    fn cancel_confirm(&mut self) {
        self.confirm = None;
        self.confirm_since = None;
        self.focus_primary = true;
    }

    /// Opens the confirm step for the commit the reader sees now. It is shown, but cannot be answered until [`ARM`] has passed, so a
    /// double click, a held key or the press that opened it never answers it.
    fn open_confirm(&mut self, which: Confirm, head: &str) {
        self.confirm = Some(which);
        self.confirm_since = Some(Instant::now());
        self.confirm_head = head.to_string();
        self.focus_confirm = true;
    }

    fn armed(&self) -> bool {
        self.confirm_since.is_some_and(|since| since.elapsed() >= ARM)
    }

    /// Draws the pane. Everything it asks for is in the returned [`Out`]; the primary action runs at most once a frame, from the
    /// button or its key, never from key repeat.
    pub(crate) fn ui(&mut self, ui: &mut Ui, pane: Rect, listed: &Mine) -> Out {
        let mut out = Out::default();
        let ctx = ui.ctx().clone();
        if listed.change.updated_at != self.seen_at || listed.head_sha != self.seen_head {
            // The list moved on (a push, a check, a review): what this pane read is out of date.
            self.seen_at = listed.change.updated_at;
            self.seen_head = listed.head_sha.clone();
            self.reload(&ctx);
        }
        let mine = self.current(listed).clone();
        if self.confirm.is_some() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.cancel_confirm();
        }
        let room = (ui.available_height() - theme::ACTION_BAR_HEIGHT).max(80.0);
        ScrollArea::vertical().id_salt("mine-body").max_height(room).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            self.head(ui, &mine);
            self.review_card(ui, &mine);
            self.checks_banner(ui, &mine, &mut out);
            self.state_card(ui, &mine);
            if mine.auto_merge && !mine.change.draft {
                self.armed_row(ui, &mine);
            }
            self.threads_section(ui, &mine);
            components::banner(
                ui,
                BannerKind::Info,
                "Babysit is not built yet",
                "An agent that watches a change until it merges is not available. Nothing here runs on its own: every fix, reply and merge starts with you.",
            );
            ui.add_space(12.0);
        });
        let clicked = self.footer(ui, pane, &mine, &mut out);
        let held = repeating(ui);
        if (clicked || keymap::fresh(ui, "mine.primary")) && !held {
            self.primary(&ctx, &mine, &mut out);
        }
        if let Some(since) = self.confirm_since.filter(|_| !self.armed()) {
            ctx.request_repaint_after(ARM.saturating_sub(since.elapsed()) + Duration::from_millis(20));
        }
        out
    }

    fn head(&self, ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        crate::detail_head(ui, &m.change, Some((TagKind::Neutral, "Your change")));
        let mut parts = vec![format!("{} → {}", m.head_branch, m.base_branch)];
        parts.extend(m.commits.map(|n| if n == 1 { "1 commit".to_string() } else { format!("{n} commits") }));
        if m.change.draft {
            parts.push("draft".into());
        } else if m.reviewers.is_empty() {
            parts.push("no reviewers".into());
        } else {
            parts.push(format!("reviewers: {}", m.reviewers.join(", ")));
        }
        components::note_sized(ui, parts.join(" · "), 13.0, p.text_secondary);
        if !m.unread.is_empty() {
            live_label(ui, format!("Not read: {}.", m.unread.join("; ")), p.ember_text);
        }
        ui.add_space(4.0);
        components::rule(ui);
    }

    /// The first failing check with a log, for View log.
    fn failing_check(&self) -> Option<String> {
        let list = &self.detail.ok()?.checks;
        list.iter().find(|c| c.state == CheckState::Failing && c.has_log).or_else(|| list.iter().find(|c| c.has_log)).map(|c| c.id.clone())
    }

    fn checks_banner(&self, ui: &mut Ui, m: &Mine, out: &mut Out) {
        let p = theme::pal(ui);
        let checks = &m.checks;
        let (fill, ink, dot, said, button) = match checks.state {
            Some(CheckState::Failing) => {
                let names = checks.failed_names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                let which = if names.is_empty() { String::new() } else { format!(": {names}") };
                let count = if checks.total > 0 { format!(" · {} of {} checks", checks.failed, checks.total) } else { String::new() };
                (p.rose_bg, p.rose_text, p.rose, format!("Checks failed{which}{count}"), "View log")
            }
            Some(CheckState::Running) => (p.haze_subtle, p.haze_text, p.haze, format!("Checks running · {}", checks.counts()), "View checks"),
            _ => return,
        };
        egui::Frame::new().fill(fill).corner_radius(8).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (rect, _) = ui.allocate_exact_size(vec2(7.0, 18.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.5, dot);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Pill::new(PillKind::Quiet, button).small().show(ui).clicked() {
                        out.checks = true;
                        out.check = self.failing_check();
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(said).size(12.5).color(ink)).truncate());
                    });
                });
            });
        });
    }

    fn state_card(&mut self, ui: &mut Ui, m: &Mine) {
        if m.change.draft {
            self.draft_card(ui, m);
        } else if m.merge == MergeState::Conflicts {
            Self::conflict_card(ui, m);
        } else {
            self.merge_card(ui, m);
        }
    }

    fn conflict_card(ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        let base = &m.base_branch;
        let body = format!(
            "Nothing is merged or pushed until you choose how to fix it. {} does not say which files conflict: open the change on the forge, or merge {base} into {} in your own checkout, to see them.",
            m.change.forge.name(),
            m.head_branch
        );
        components::banner(ui, BannerKind::Error, &format!("This branch conflicts with {base}"), &body);
        components::group_label(ui, "Conflicting files");
        components::note(ui, "Not available: neither GitHub nor GitLab names the files that conflict.", p.text_secondary);
        components::note(ui, "Drafting a fix with the AI engine is not built yet, so nothing here changes the branch.", p.text_secondary);
    }

    fn start_pickers(&mut self, ctx: &Context) {
        if !self.live {
            return;
        }
        if matches!(self.people, Load::Idle) {
            let c = self.change.clone();
            self.people = Load::start(ctx, move || engine::pr_collaborators(&c).map_err(|e| e.to_string()));
        }
        if matches!(self.labels, Load::Idle) {
            let c = self.change.clone();
            self.labels = Load::start(ctx, move || engine::pr_labels(&c).map_err(|e| e.to_string()));
        }
    }

    fn draft_card(&mut self, ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        self.start_pickers(ui.ctx());
        components::banner(
            ui,
            BannerKind::Info,
            "This is a draft",
            "Reviewers are not notified and it cannot merge. Mark it ready when you want eyes on it.",
        );
        let mut picks = self.picks.clone().unwrap_or_default();
        components::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            components::label(ui, "Reviewers", 13.0, p.graphite);
            self.reviewer_rows(ui, m, &mut picks);
            ui.add_space(8.0);
            components::label(ui, "Labels", 13.0, p.graphite);
            self.label_rows(ui, &mut picks);
            if let Some(detail) = self.detail.ok().filter(|d| !d.closes.is_empty()) {
                ui.add_space(4.0);
                let issues: Vec<String> = detail.closes.iter().map(|i| format!("#{} {}", i.number, i.title)).collect();
                components::note(ui, format!("Merging closes {}", issues.join(", ")), p.text_secondary);
            }
        });
        let forge = m.change.forge.name();
        components::note(ui, format!("Reviewers and labels are sent to {forge} when you press either button below."), p.text_tertiary);
        self.picks = Some(picks);
    }

    fn reviewer_rows(&self, ui: &mut Ui, m: &Mine, picks: &mut Picks) {
        let p = theme::pal(ui);
        match (&self.people, self.live) {
            (Load::Ready(Ok(page)), _) => {
                let people: Vec<&Person> = page.items.iter().filter(|x| !x.login.eq_ignore_ascii_case(&m.change.author)).collect();
                if people.is_empty() {
                    components::note(ui, "The forge lists nobody else who can review here.", p.text_secondary);
                }
                ScrollArea::vertical().id_salt("reviewer-rows").max_height(176.0).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for person in people {
                        let on = picks.reviewers.iter().any(|r| r.eq_ignore_ascii_case(&person.login));
                        if components::check_row(ui, on, &person.login, &person.role, "").clicked() {
                            picks.reviewers.retain(|r| !r.eq_ignore_ascii_case(&person.login));
                            if !on {
                                picks.reviewers.push(person.login.clone());
                            }
                        }
                    }
                });
                if page.truncated {
                    components::note(ui, format!("The list holds the first {} people; more can review.", pr_craft_forge::PAGE), p.text_tertiary);
                }
            }
            (Load::Ready(Err(e)), _) => live_label(ui, format!("Could not read who can review: {e}"), p.rose_text),
            (_, true) => {
                components::note(ui, "Reading who can review…", p.text_tertiary);
            }
            _ => {
                components::note(ui, "Who can review is read from the forge when the app is live.", p.text_tertiary);
            }
        }
    }

    fn label_rows(&self, ui: &mut Ui, picks: &mut Picks) {
        let p = theme::pal(ui);
        match &self.labels {
            Load::Ready(Ok(page)) => {
                ScrollArea::vertical().id_salt("label-rows").max_height(96.0).auto_shrink([false, true]).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for label in &page.items {
                            let on = picks.labels.iter().any(|l| l == &label.name);
                            let text = if on { format!("✓ {}", label.name) } else { label.name.clone() };
                            let kind = if on { PillKind::Secondary } else { PillKind::Quiet };
                            if Pill::new(kind, &text).small().show(ui).clicked() {
                                picks.labels.retain(|l| l != &label.name);
                                if !on {
                                    picks.labels.push(label.name.clone());
                                }
                            }
                        }
                    });
                });
                if page.items.is_empty() {
                    components::note(ui, "The repository has no labels.", p.text_secondary);
                }
                if page.truncated {
                    components::note(ui, format!("The list holds the first {} labels; more exist.", pr_craft_forge::PAGE), p.text_tertiary);
                }
            }
            Load::Ready(Err(e)) => live_label(ui, format!("Could not read the labels: {e}"), p.rose_text),
            _ => {
                components::note(ui, "Reading the labels…", p.text_tertiary);
            }
        }
    }

    fn approvals_text(m: &Mine) -> String {
        match (m.approvals, m.approvals_required) {
            (a, Some(n)) => format!("{a} of {n} approvals"),
            (0, None) => "No approvals yet".to_string(),
            (1, None) => "1 approval".to_string(),
            (a, None) => format!("{a} approvals"),
        }
    }

    fn merge_card(&mut self, ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        let title = match m.merge {
            MergeState::Ready => "Ready to merge".to_string(),
            MergeState::Behind => format!("{} has moved on", m.base_branch),
            MergeState::Blocked => "Not ready to merge".to_string(),
            _ => "Merge state not known yet".to_string(),
        };
        let stroke = Stroke::new(1.5, p.graphite);
        egui::Frame::new().fill(p.raised).stroke(stroke).corner_radius(8).inner_margin(16).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            let counts = [Some(Self::approvals_text(m)), Some(m.checks.counts()).filter(|c| !c.is_empty())];
            let line = counts.into_iter().flatten().collect::<Vec<_>>().join(" · ");
            let good = m.merge == MergeState::Ready && m.decision != Decision::ChangesRequested;
            let (fill, ink) = if good { (p.moss_bg, p.moss_text) } else { (p.hover, p.text_secondary) };
            if ui.available_width() < 520.0 {
                // Beside the title the chip would cover it.
                components::label(ui, title, 15.0, p.graphite);
                chip(ui, fill, ink, &line);
            } else {
                ui.horizontal(|ui| {
                    components::label(ui, title, 15.0, p.graphite);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| chip(ui, fill, ink, &line));
                });
            }
            if let Some(note) = &m.merge_note {
                components::note(ui, format!("{note}."), p.text_secondary);
            }
            if m.merge == MergeState::Behind {
                components::note(
                    ui,
                    format!("The branch is behind {}; the forge may ask you to update it before it merges.", m.base_branch),
                    p.text_secondary,
                );
            }
            if Self::merge_open(m) {
                self.merge_options(ui, m);
            } else {
                let why = if m.decision == Decision::ChangesRequested { "A reviewer asked for changes" } else { "Checks are failing" };
                components::note(
                    ui,
                    format!("{why}, so there is nothing to choose yet. Merge options appear when that is dealt with."),
                    p.text_secondary,
                );
            }
        });
        self.confirm_card(ui, m);
    }

    fn merge_options(&mut self, ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        let Some(detail) = self.detail.ok().cloned() else {
            match self.detail.ready() {
                Some(Err(e)) => live_label(ui, format!("Could not read the merge options: {e}"), p.rose_text),
                _ => {
                    components::note(ui, "Reading the merge options…", p.text_tertiary);
                }
            }
            return;
        };
        components::label(ui, "Method", 12.0, p.text_secondary);
        let labels: Vec<String> = detail.methods.iter().map(|x| x.label().to_string()).collect();
        let at = detail.methods.iter().position(|x| Some(*x) == self.form.method).unwrap_or(0);
        if let Some(i) = components::segmented(ui, "merge-method", "Merge method", &labels, at) {
            self.form.method = detail.methods.get(i).copied();
        }
        if self.form.method != Some(MergeMethod::Rebase) {
            components::label(ui, "Commit message", 12.0, p.text_secondary);
            let field =
                ui.add(TextEdit::multiline(&mut self.form.message).font(egui::FontId::monospace(12.0)).desired_width(f32::INFINITY).desired_rows(3));
            components::name_field(ui, &field, "Commit message");
            self.form.edited |= field.changed();
        }
        let delete = format!("Delete {} after merge", m.head_branch);
        if components::check_row(ui, self.form.delete_branch, &delete, "", "").clicked() {
            self.form.delete_branch = !self.form.delete_branch;
        }
        if !detail.closes.is_empty() {
            let issues: Vec<String> = detail.closes.iter().map(|i| format!("#{} {}", i.number, i.title)).collect();
            components::note(ui, format!("Merging closes {}", issues.join(", ")), p.text_secondary);
        }
        if let Some(e) = &detail.queue_error {
            let said = format!("Could not read whether {} has a merge queue: {e}. Merging stays off until that is known.", m.base_branch);
            live_label(ui, said, p.rose_text);
        }
        if let Some(e) = &detail.checks_error {
            live_label(ui, format!("Could not read the checks: {e}"), p.rose_text);
        }
        if let Some(queue) = &detail.queue {
            self.queue_box(ui, m, queue);
        }
        if !m.auto_merge {
            self.auto_row(ui, m);
        }
    }

    fn queue_box(&self, ui: &mut Ui, m: &Mine, queue: &pr_craft_core::mine::MergeQueue) {
        let p = theme::pal(ui);
        let ahead = match queue.ahead {
            0 => "Nothing ahead".to_string(),
            1 => "1 change ahead".to_string(),
            n => format!("{n} changes ahead"),
        };
        let eta = queue.estimate_secs.map(|s| format!(" · about {} min", s.div_ceil(60))).unwrap_or_default();
        let state = if queue.queued { "In the queue · " } else { "" };
        egui::Frame::new().fill(p.haze_subtle).corner_radius(8).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(format!("{} uses a merge queue", m.base_branch)).size(12.5).color(p.haze_text).strong()).wrap());
            let line = format!("{state}{ahead}{eta}. CI runs again on the queued result.");
            ui.add(egui::Label::new(RichText::new(line).size(12.0).color(p.haze_text)).wrap());
        });
    }

    fn auto_row(&mut self, ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                components::label(ui, "Merge when ready", 13.0, p.graphite);
                components::note(ui, "If a check is still running, merge as soon as it passes.", p.text_secondary);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let busy = self.acting.busy();
                ui.add_enabled_ui(!busy, |ui| match components::toggle(ui, m.auto_merge, "Merge when ready") {
                    Some(true) => self.open_confirm(Confirm::Auto, &m.head_sha),
                    Some(false) => self.send_merge(ui.ctx(), &m.head_sha, None),
                    None => {}
                });
            });
        });
    }

    /// An armed Merge when ready, in every state: it will merge when the blocker clears, so it is never hidden and can be turned off.
    fn armed_row(&mut self, ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        egui::Frame::new().fill(p.moss_bg).corner_radius(8).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    components::label(ui, "Merge when ready · on", 13.0, p.moss_text);
                    let line = format!("{} merges into {} when its checks pass and nothing else blocks it.", m.change.reference(), m.base_branch);
                    ui.add(egui::Label::new(RichText::new(line).size(12.0).color(p.moss_text)).wrap());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let busy = self.acting.busy();
                    ui.add_enabled_ui(!busy, |ui| {
                        if components::toggle(ui, true, "Merge when ready") == Some(false) {
                            self.send_merge(ui.ctx(), &m.head_sha, None);
                        }
                    });
                });
            });
        });
    }

    fn method(&self) -> MergeMethod {
        self.form.method.unwrap_or(MergeMethod::Squash)
    }

    fn confirm_card(&self, ui: &mut Ui, m: &Mine) {
        let Some(which) = self.confirm else { return };
        let (what, base, method) = (m.change.reference(), &m.base_branch, self.method());
        let delete = if self.form.delete_branch { format!(" {} is deleted afterward.", m.head_branch) } else { String::new() };
        let (title, body) = match which {
            Confirm::Merge => (
                format!("Merge into {base} with {}?", method.label()),
                format!(
                    "{} {} {what} into {base}: it {}.{delete} A merge cannot be undone.",
                    m.change.forge.name(),
                    method.label().to_lowercase(),
                    method.does()
                ),
            ),
            Confirm::Queue => (
                format!("Add to the merge queue of {base}?"),
                format!(
                    "{what} joins the queue of {base} ({}); the forge merges it when its turn comes, after CI runs again on the queued result.",
                    method.label()
                ),
            ),
            Confirm::Auto => (
                format!("Merge into {base} when its checks pass?"),
                format!(
                    "{what} merges into {base} ({}) as soon as its checks pass, with no further question. Turn Merge when ready off here before then to stop it.",
                    method.label()
                ),
            ),
        };
        let said = components::banner(ui, BannerKind::Warn, &title, &body);
        ui.ctx().accesskit_node_builder(said.id, |node| node.set_live(egui::accesskit::Live::Polite));
    }

    fn review_card(&self, ui: &mut Ui, m: &Mine) {
        if m.decision != Decision::ChangesRequested || m.change.draft {
            return;
        }
        let first = m.changes_requested.first();
        let who = first.map_or("A reviewer", |c| c.by.as_str());
        let when = first.and_then(|c| c.at).map(|at| format!(" {} ago", age(Utc::now(), at))).unwrap_or_default();
        let resolved = self.threads.ok().map(|t| t.threads.iter().filter(|x| x.resolved).count());
        let mut body = match (m.threads_waiting, m.threads_open) {
            (_, 0) => "No thread is open.".to_string(),
            (0, open) => format!("{open} thread{} open, none waiting on you.", if open == 1 { "" } else { "s" }),
            (w, _) => format!("{w} thread{} wait{} on you.", if w == 1 { "" } else { "s" }, if w == 1 { "s" } else { "" }),
        };
        if let Some(n) = resolved.filter(|n| *n > 0) {
            body.push_str(&format!(" {n} resolved."));
        }
        if let Some(one) = m.checks.failed_names.first() {
            body.push_str(&format!(" Checks are failing on {one}."));
        }
        body.push_str(" Push a fix or reply, then ask for another look.");
        components::banner(ui, BannerKind::Warn, &format!("{who} requested changes{when}"), &body);
    }

    fn threads_section(&self, ui: &mut Ui, m: &Mine) {
        let p = theme::pal(ui);
        if m.change.draft
            || (m.threads_open == 0 && m.decision != Decision::ChangesRequested && self.threads.ok().is_none_or(|t| t.threads.is_empty()))
        {
            return;
        }
        components::group_label(ui, "Threads");
        match &self.threads {
            Load::Ready(Ok(list)) => {
                let mut sorted: Vec<&ReviewThread> = list.threads.iter().collect();
                sorted.sort_by_key(|t| (t.resolved, !Self::waits_on_you(t, m)));
                for thread in sorted {
                    Self::thread_card(ui, thread, m);
                }
                if !list.complete {
                    components::note(ui, list.note.clone().unwrap_or_else(|| "Some threads or comments were not read.".into()), p.text_secondary);
                }
            }
            Load::Ready(Err(e)) => live_label(ui, format!("Could not read the threads: {e}"), p.rose_text),
            _ => {
                components::note(ui, "Reading the threads…", p.text_tertiary);
            }
        }
    }

    fn waits_on_you(thread: &ReviewThread, m: &Mine) -> bool {
        !thread.resolved && thread.comments.last().is_some_and(|c| !c.author.eq_ignore_ascii_case(&m.change.author))
    }

    fn thread_card(ui: &mut Ui, thread: &ReviewThread, m: &Mine) {
        let p = theme::pal(ui);
        let place = match thread.line.and_then(|l| l.new.or(l.old)) {
            Some(line) => format!("{}:{line}", thread.path),
            None => thread.path.clone(),
        };
        egui::Frame::new().fill(p.raised).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(Margin::symmetric(12, 10)).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    components::mono_label(ui, place, 12.0, p.text_secondary);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if thread.resolved {
                            chip(ui, p.moss_bg, p.moss_text, "Resolved");
                        } else if Self::waits_on_you(thread, m) {
                            components::tag(ui, TagKind::YourTurn, "Waiting on you");
                        } else {
                            let other = thread.comments.first().map_or("the reviewer", |c| c.author.as_str());
                            components::tag(ui, TagKind::Neutral, &format!("Waiting on {other}"));
                        }
                    });
                });
                let shown: Vec<_> = match thread.comments.as_slice() {
                    [] => Vec::new(),
                    [only] => vec![only],
                    [first, .., last] => vec![first, last],
                };
                for comment in shown {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        components::avatar(ui, &comment.author, 20.0);
                        let line = comment.body.lines().next().unwrap_or_default().to_string();
                        ui.add(egui::Label::new(RichText::new(line).size(13.0).color(p.graphite)).truncate());
                    });
                }
                if thread.comments_total as usize > thread.comments.len() {
                    components::note(ui, format!("{} in all.", thread.comment_count()), p.text_tertiary);
                }
            },
        );
    }

    fn can_ask_again(m: &Mine) -> bool {
        m.change.forge == Forge::GitHub && m.decision == Decision::ChangesRequested && !m.changes_requested.is_empty()
    }

    fn send_ready(&mut self, ctx: &Context, ready: bool) {
        let (change, seed) = (self.change.clone(), self.seed.clone());
        let picks = self.picks.clone().unwrap_or_default();
        self.run(ctx, move || ready_work(&change, &seed, &picks, ready));
    }

    fn ask_again(&mut self, ctx: &Context, m: &Mine) {
        let Some(who) = m.changes_requested.first().map(|c| c.by.clone()) else { return };
        let change = self.change.clone();
        // Added to who is asked right now, so a reviewer added since the pane was read stays.
        self.run(ctx, move || {
            engine::pr_change_reviewers(&change, std::slice::from_ref(&who), &[])
                .map(|_| format!("Asked {who} for another look at {}.", change.reference()))
                .map_err(|e| e.to_string())
        });
    }

    /// Sends the merge (`Some`), or turns Merge when ready off (`None`).
    fn send_merge(&mut self, ctx: &Context, head_sha: &str, which: Option<Merge>) {
        let message = if self.method() == MergeMethod::Rebase { String::new() } else { self.form.message.clone() };
        let ask = MergeAsk {
            change: self.change.clone(),
            head_sha: head_sha.to_string(),
            method: Some(self.method()),
            message,
            delete_branch: self.form.delete_branch,
            enabled: which.is_some(),
            confirm: which.is_some(),
        };
        self.run(ctx, move || engine::merge(which.unwrap_or(Merge::Auto), ask).map(|d| d.message).map_err(|e| e.to_string()));
    }

    /// Whether the merge can be asked for at all: a failing check or a reviewer who asked for changes stops it first, so there are
    /// no merge options to show until that is dealt with.
    fn merge_open(m: &Mine) -> bool {
        m.checks.state != Some(CheckState::Failing) && m.decision != Decision::ChangesRequested
    }

    fn kind(&self, m: &Mine) -> Kind {
        if self.confirm.is_some() {
            Kind::Confirm
        } else if m.change.draft {
            Kind::Ready
        } else if m.merge == MergeState::Conflicts {
            Kind::Conflict
        } else if Self::can_ask_again(m) {
            Kind::Ask
        } else if !Self::merge_open(m) {
            Kind::Review
        } else if self.detail.ok().is_some_and(|d| d.queue.is_some()) {
            Kind::Queue
        } else {
            Kind::Merge
        }
    }

    /// What the primary button and its key do now.
    fn primary(&mut self, ctx: &Context, m: &Mine, out: &mut Out) {
        if self.acting.busy() {
            return;
        }
        match self.kind(m) {
            Kind::Confirm => {
                // Not before the reader has had time to read it.
                if !self.armed() {
                    return;
                }
                let merge = match self.confirm {
                    Some(Confirm::Queue) => Merge::Queue,
                    Some(Confirm::Auto) => Merge::Auto,
                    _ => Merge::Start,
                };
                let head = self.confirm_head.clone();
                self.send_merge(ctx, &head, Some(merge));
            }
            Kind::Ready => self.send_ready(ctx, true),
            Kind::Conflict => ctx.open_url(OpenUrl::new_tab(&m.change.url)),
            Kind::Ask => self.ask_again(ctx, m),
            Kind::Review => out.review = true,
            Kind::Merge | Kind::Queue if self.can_merge(m) => {
                let which = if self.kind(m) == Kind::Queue { Confirm::Queue } else { Confirm::Merge };
                self.open_confirm(which, &m.head_sha);
            }
            Kind::Merge | Kind::Queue => {}
        }
    }

    /// The merge is ready and nothing says it is already queued.
    fn can_merge(&self, m: &Mine) -> bool {
        m.merge == MergeState::Ready
            && !m.change.draft
            && self.detail.ok().is_some_and(|d| d.queue_error.is_none() && d.queue.as_ref().is_none_or(|q| !q.queued))
    }

    fn primary_label(&self, m: &Mine) -> String {
        match (self.kind(m), self.confirm) {
            (Kind::Confirm, Some(Confirm::Merge)) => format!("Merge into {}", m.base_branch),
            (Kind::Confirm, Some(Confirm::Queue)) => "Add to the queue".to_string(),
            (Kind::Confirm, _) => "Turn on".to_string(),
            (Kind::Ready, _) => match self.picks.as_ref().map_or(0, |p| p.reviewers.len()) {
                0 => "Ready for review".to_string(),
                1 => "Ready for review · ask 1 person".to_string(),
                n => format!("Ready for review · ask {n} people"),
            },
            (Kind::Conflict, _) => format!("Open on {}", m.change.forge.name()),
            (Kind::Ask, _) => format!("Ask {} for another look", m.changes_requested.first().map_or("the reviewer", |c| c.by.as_str())),
            (Kind::Review, _) => "Open review".to_string(),
            (Kind::Queue, _) if self.detail.ok().and_then(|d| d.queue.as_ref()).is_some_and(|q| q.queued) => "In the merge queue".to_string(),
            (Kind::Queue, _) => "Add to merge queue".to_string(),
            (Kind::Merge, _) => self.method().label().to_string(),
        }
    }

    /// Whether the primary button can be pressed now.
    fn primary_enabled(&self, m: &Mine) -> bool {
        let idle = !self.acting.busy();
        match self.kind(m) {
            Kind::Merge | Kind::Queue => idle && self.can_merge(m),
            Kind::Confirm => idle && self.armed(),
            _ => idle,
        }
    }

    /// Draws the action bar. True when the primary button was pressed (the caller runs it once with the key, so a press of both in one
    /// frame is one press).
    fn footer(&mut self, ui: &mut Ui, pane: Rect, m: &Mine, out: &mut Out) -> bool {
        let rect = Rect::from_min_max(egui::pos2(pane.left(), pane.bottom() - theme::ACTION_BAR_HEIGHT), pane.right_bottom());
        components::seam_top(ui, rect);
        let inner = rect.shrink2(vec2(32.0, 0.0));
        let p = theme::pal(ui);
        let mut clicked = false;
        ui.scope_builder(UiBuilder::new().id_salt("mine-actions").max_rect(inner).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let mut room = inner.width();
            let ctx = ui.ctx().clone();
            let label = self.primary_label(m);
            let primary = Pill::new(PillKind::Primary, &label).key(ui, "mine.primary").enabled(self.primary_enabled(m));
            room -= primary.width(ui) + 8.0;
            let shown = primary.show(ui);
            clicked = shown.clicked();
            if self.confirm.is_some() && self.armed() && self.focus_confirm {
                shown.request_focus();
                self.focus_confirm = false;
            }
            if self.confirm.is_none() && self.focus_primary {
                shown.request_focus();
                self.focus_primary = false;
            }
            if self.confirm.is_some() {
                if Pill::new(PillKind::Secondary, "Cancel").show(ui).clicked() {
                    self.cancel_confirm();
                }
                if !self.armed() {
                    components::note(ui, "Check it, then confirm", p.text_tertiary);
                }
                return;
            }
            if m.change.draft {
                let changed =
                    self.picks.as_ref().is_some_and(|p| !same_set(&p.reviewers, &self.seed.reviewers) || !same_set(&p.labels, &self.seed.labels));
                let keep = Pill::new(PillKind::Secondary, "Keep as draft").enabled(changed && !self.acting.busy());
                room -= keep.width(ui) + 8.0;
                if keep.show(ui).clicked() {
                    self.send_ready(&ctx, false);
                }
            }
            if room > 130.0 && Pill::new(PillKind::Secondary, "Copy branch").show(ui).clicked() {
                ctx.copy_text(m.head_branch.clone());
            }
            room -= 130.0;
            let kind = self.kind(m);
            if kind == Kind::Conflict && room > 250.0 {
                // The AI engine's conflict fix is not built; the button says so instead of being left out.
                let draft = Pill::new(PillKind::Quiet, "Draft a fix · not built yet").enabled(false);
                room -= draft.width(ui) + 8.0;
                draft.show(ui);
            }
            if room > 130.0
                && !m.change.draft
                && kind != Kind::Review
                && Pill::new(PillKind::Secondary, "Open review").key(ui, "inbox.open").show(ui).clicked()
            {
                out.review = true;
            }
            if self.acting.busy() {
                components::note(ui, format!("Sending to {}…", m.change.forge.name()), p.text_tertiary);
            }
        });
        clicked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn only_the_readers_own_edit_is_sent_whatever_changed_on_the_forge_meanwhile() {
        // The reader never touched the labels, and a bot added one after the pane opened and the pane re-seeded.
        let seed = names(&["size/XS"]);
        assert_eq!(delta(&seed, &seed), (Vec::new(), Vec::new()), "nothing to send, so nothing is removed");
        // The reader added "bug" and removed "docs".
        let (add, remove) = delta(&names(&["docs"]), &names(&["bug"]));
        assert_eq!((add, remove), (names(&["bug"]), names(&["docs"])));
    }

    #[test]
    fn a_reload_keeps_the_readers_edit_on_top_of_what_the_forge_says_now() {
        let old_seed = names(&["a", "b"]);
        let picks = names(&["a", "c"]);
        let fresh = names(&["a", "b", "size/XS"]);
        let now = rebased(&old_seed, &picks, &fresh);
        assert!(now.contains(&"size/XS".to_string()), "what someone else added is there");
        assert!(now.contains(&"c".to_string()), "what the reader added is kept");
        assert!(!now.contains(&"b".to_string()), "what the reader removed stays removed");
        assert_eq!(delta(&fresh, &now), (names(&["c"]), names(&["b"])), "and the next send is still only the reader's edit");
    }
}
