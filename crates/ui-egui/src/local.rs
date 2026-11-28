//! Open locally (Figma frame 96): check the change out in a worktree or in the clone the user named, and open the file at the
//! line in an editor. Each choice is an engine command (`checkout.status`, `checkout.worktree`, `checkout.remove`, `checkout.here`, `open.editor`);
//! this screen draws what they answer, and every refusal in the engine's own words.

use std::cell::Cell;

use craft::egui::{self, Align2, Context, Margin, Rect, Ui};
use pr_craft_core::ChangeRef;
use pr_craft_core::editor::Editor;
use pr_craft_engine::local::{self, Status};

use crate::ai::Fetch;
use crate::components::{self, BannerKind, Pill, PillKind};
use crate::keymap;
use crate::theme;

/// What the popover asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalAct {
    Toggle,
    Worktree,
    /// "Remove the worktree", offered once the change is merged or closed.
    Remove,
    /// "Check out here": asks first.
    AskHere,
    ConfirmHere,
    CancelConfirm,
    Editor(Editor),
}

/// The file and line the editors open: the selected line, else the top of the current hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub path: String,
    pub line: u32,
}

/// The clone and the branch the confirm step names; "Check out here" runs only after the reader said yes to exactly these.
struct Confirm {
    folder: String,
    from: String,
    branch: String,
    /// Focus Cancel on the frame the step opens only.
    focus_pending: Cell<bool>,
}

pub(crate) struct Local {
    pub open: bool,
    status: Fetch<Status>,
    /// A worktree, a switch or an editor on its way.
    work: Fetch<String>,
    confirm: Option<Confirm>,
    /// The last answer of a choice: news, or the reason it did not run.
    said: Option<Result<String, String>>,
    /// Where the Open control was drawn, so a press on it does not count as "outside" the popover.
    pub trigger: Cell<Option<Rect>>,
}

/// `~/code/api` for a path under the home folder.
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => path
            .strip_prefix(&home)
            .filter(|rest| rest.is_empty() || rest.starts_with('/'))
            .map_or_else(|| path.to_string(), |rest| format!("~{rest}")),
        _ => path.to_string(),
    }
}

impl Local {
    pub(crate) fn new() -> Self {
        Local { open: false, status: Fetch::Idle, work: Fetch::Idle, confirm: None, said: None, trigger: Cell::new(None) }
    }

    fn reload(&mut self, ctx: &Context, change: &ChangeRef) {
        let change = change.clone();
        self.status = Fetch::start(ctx, move || local::status(&change).map_err(crate::fail));
    }

    pub(crate) fn busy(&self) -> bool {
        self.work.loading()
    }

    /// Takes in what the threads have answered. A finished choice is said, and the status is read again, so the rows show the
    /// worktree it made or the branch it switched to.
    pub(crate) fn poll(&mut self, ctx: &Context, change: &ChangeRef) {
        self.status.poll();
        self.work.poll();
        let done = match std::mem::replace(&mut self.work, Fetch::Idle) {
            Fetch::Ready(news) => Some(Ok(news)),
            Fetch::Failed(error) => Some(Err(error)),
            other => {
                self.work = other;
                None
            }
        };
        if done.is_some() {
            self.said = done;
            self.reload(ctx, change);
        }
    }

    /// The loaded status, when the clone and the checkouts folder have been read.
    fn ready(&self) -> Option<&Status> {
        match &self.status {
            Fetch::Ready(status) => Some(status),
            _ => None,
        }
    }

    pub(crate) fn act(&mut self, ctx: &Context, change: &ChangeRef, act: LocalAct, target: Option<&Target>) {
        if act != LocalAct::Toggle && act != LocalAct::CancelConfirm && !self.open {
            self.open = true;
            self.reload(ctx, change);
        }
        match act {
            LocalAct::Toggle => {
                self.open = !self.open;
                self.confirm = None;
                self.said = None;
                if self.open {
                    self.reload(ctx, change);
                }
            }
            LocalAct::CancelConfirm => self.confirm = None,
            _ if self.work.loading() => self.said = Some(Err("Wait for the last choice to finish.".into())),
            LocalAct::Worktree => {
                self.said = None;
                let change = change.clone();
                self.work = Fetch::start(ctx, move || {
                    let made = local::worktree(&change).map_err(crate::fail)?;
                    let at = tilde(&made.path);
                    Ok(if made.reused {
                        format!("Reused the worktree at {at}.{}", other_commit_words(made.other_commit))
                    } else {
                        format!("Made a worktree at {at}. Your branch stays put.")
                    })
                });
            }
            LocalAct::Remove => {
                self.said = None;
                let change = change.clone();
                self.work = Fetch::start(ctx, move || {
                    let done = local::remove(&change).map_err(crate::fail)?;
                    Ok(format!("Removed the worktree at {}.", tilde(&done.removed)))
                });
            }
            LocalAct::AskHere => self.ask_here(),
            LocalAct::ConfirmHere => {
                let Some(confirm) = self.confirm.take() else { return };
                let change = change.clone();
                self.work = Fetch::start(ctx, move || {
                    let done = local::here(&change, true, Some(&confirm.branch)).map_err(crate::fail)?;
                    Ok(format!("Switched {} to {}.", confirm.folder, done.branch.unwrap_or(confirm.branch)))
                });
            }
            LocalAct::Editor(editor) => self.open_in(ctx, change, editor, target),
        }
    }

    fn ask_here(&mut self) {
        self.said = None;
        let Some(status) = self.ready() else {
            self.said = Some(Err("Still reading your clone. Try again in a moment.".into()));
            return;
        };
        if let Some(why) = &status.here_action.why_not {
            self.said = Some(Err(why.clone()));
            return;
        }
        let (Some(clone), Some(branch)) = (&status.clone, &status.head_branch) else { return };
        let from = clone.state.as_ref().and_then(|s| s.branch.clone()).unwrap_or_else(|| "a detached commit".into());
        self.confirm = Some(Confirm { folder: tilde(&clone.path), from, branch: branch.clone(), focus_pending: Cell::new(true) });
    }

    fn open_in(&mut self, ctx: &Context, change: &ChangeRef, editor: Editor, target: Option<&Target>) {
        self.said = None;
        let Some(status) = self.ready() else {
            self.said = Some(Err("Still reading your clone. Try again in a moment.".into()));
            return;
        };
        if status.editor_root.is_none() {
            self.said = Some(Err(format!(
                "This change is not checked out on this Mac, so there is nothing to open. Check it out in a worktree first ({}).",
                keymap::label(ctx, "review.worktree")
            )));
            return;
        }
        let file = target.cloned().filter(|_| editor != Editor::Terminal);
        if file.is_none() && editor != Editor::Terminal {
            self.said = Some(Err("Select a changed line first: the editor opens that file at that line.".into()));
            return;
        }
        let change = change.clone();
        self.work = Fetch::start(ctx, move || {
            let opened =
                local::open_editor(&change, editor, file.as_ref().map(|t| t.path.as_str()), file.as_ref().map(|t| t.line)).map_err(crate::fail)?;
            let news = match file {
                Some(t) => format!("Opened {}:{} in {}.", t.path, t.line, editor.label()),
                None => format!("Opened {} in {}.", tilde(&opened.root), editor.label()),
            };
            Ok(match opened.note {
                Some(note) => format!("{news} {note}"),
                None => news,
            })
        });
    }

    /// Whether the confirm step is showing.
    pub(crate) fn confirming(&self) -> bool {
        self.confirm.is_some()
    }

    /// The popover under the Open control: the choices, each with the command that runs, and the answer of the last one.
    pub(crate) fn popover(&self, ctx: &Context, change: &ChangeRef, target: Option<&Target>, finished: bool, acts: &mut Vec<LocalAct>) -> Rect {
        let (screen, p) = (ctx.content_rect(), theme::pal(ctx));
        let anchor = self.trigger.get().map_or(egui::pos2(screen.right() - 16.0, 44.0), |t| egui::pos2(t.right(), t.bottom() + 4.0));
        let area = egui::Area::new(egui::Id::NULL.with("open-locally"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(egui::pos2(anchor.x.max(theme::POPOVER_WIDTH + 8.0), anchor.y))
            .show(ctx, |ui| {
                components::card_frame(ui).inner_margin(Margin::same(6)).show(ui, |ui| {
                    ui.set_width(theme::POPOVER_WIDTH);
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        let name = format!("Open {}{} locally", change.forge.number_sigil(), change.number);
                        components::label(ui, name, 12.5, p.graphite);
                        components::key_cap(ui, &keymap::label(ui, "review.open_locally"));
                    });
                    ui.add_space(theme::SPACE_2);
                    match &self.confirm {
                        Some(confirm) => self.confirm_step(ui, confirm, acts),
                        None => self.choices(ui, target, finished, acts),
                    }
                    self.answer(ui);
                });
            });
        area.response.rect
    }

    fn note_row(ui: &mut Ui, text: &str, colour: egui::Color32) {
        components::padded(ui, Margin::symmetric(6, 2), |ui| {
            components::note(ui, text, colour);
        });
    }

    fn choices(&self, ui: &mut Ui, target: Option<&Target>, finished: bool, acts: &mut Vec<LocalAct>) {
        let p = theme::pal(ui);
        let status = match &self.status {
            Fetch::Ready(status) => status,
            Fetch::Failed(error) => return Self::note_row(ui, &format!("Could not read this Mac: {error}"), p.rose_text),
            Fetch::Idle | Fetch::Loading(_) => return Self::note_row(ui, "Reading your clone…", p.text_secondary),
        };
        if let Some(setup) = &status.setup {
            components::banner(ui, BannerKind::Info, "No local clone yet", setup);
            ui.add_space(theme::SPACE_2);
        }
        let worktree_line = self.worktree_line(status);
        let line = status.worktree_action.why_not.as_deref().unwrap_or(&worktree_line);
        if components::local_row(ui, "Check out in a worktree", line, &keymap::label(ui, "review.worktree"), true, status.worktree_action.allowed)
            .clicked()
        {
            acts.push(LocalAct::Worktree);
        }
        if let Some(path) = status.worktree.as_ref().filter(|_| finished)
            && components::local_row(ui, "Remove the worktree", &tilde(path), "", false, true).clicked()
        {
            acts.push(LocalAct::Remove);
        }
        let here_line = match (&status.clone, &status.head_branch, status.here_action.why_not.as_deref()) {
            (_, _, Some(why)) => why.to_string(),
            (Some(clone), Some(branch), None) => format!(
                "{} {} checkout → switches {} to {branch}",
                status.change.forge.cli(),
                if status.change.forge == pr_craft_core::Forge::GitHub { "pr" } else { "mr" },
                tilde(&clone.path)
            ),
            _ => String::new(),
        };
        if components::local_row(ui, "Check out here", &here_line, "", false, status.here_action.allowed).clicked() {
            acts.push(LocalAct::AskHere);
        }
        let checked_out = status.editor_root.is_some();
        for editor in Editor::ALL {
            let on = checked_out && (editor == Editor::Terminal || target.is_some());
            let title = match (editor, target) {
                (Editor::Terminal, _) => "Open Terminal at the checkout".to_string(),
                (Editor::VsCode, Some(t)) => format!("Open {}:{} in {}", t.path.rsplit('/').next().unwrap_or(&t.path), t.line, editor.label()),
                _ => format!("Open in {}", editor.label()),
            };
            let line = if on || editor != Editor::VsCode { editor.hint() } else { editor_why(checked_out, target) };
            let cap = if editor == Editor::VsCode { keymap::label(ui, "review.open_locally") } else { String::new() };
            if components::local_row(ui, &title, line, &cap, false, on).clicked() {
                acts.push(LocalAct::Editor(editor));
            }
        }
    }

    /// The line under "Check out in a worktree": the real commands, or the worktree that is already there.
    fn worktree_line(&self, status: &Status) -> String {
        match (&status.worktree, &status.worktree_path) {
            (Some(path), _) => {
                format!("reuses {}{}", tilde(path), if status.worktree_other_commit { " · at a different commit than the PR head" } else { "" })
            }
            (None, Some(path)) => format!("git fetch, git worktree add --detach → {}", tilde(path)),
            (None, None) => String::new(),
        }
    }

    fn confirm_step(&self, ui: &mut Ui, confirm: &Confirm, acts: &mut Vec<LocalAct>) {
        let p = theme::pal(ui);
        components::padded(ui, Margin::symmetric(6, 2), |ui| {
            let heading = components::label(ui, "Check out here?", 13.5, p.graphite);
            ui.ctx().accesskit_node_builder(heading.id, |node| node.set_live(egui::accesskit::Live::Polite));
            ui.add_space(theme::SPACE_2);
            components::body_text(ui, &format!("This switches the clone {} from {} to {}.", confirm.folder, confirm.from, confirm.branch));
            components::note(
                ui,
                "This is checked again when you confirm: uncommitted changes, conflicts, and untracked or ignored files the switch would overwrite stop it. Git hooks do not run for this switch, so a hook such as git-lfs's post-checkout does not either. PR Craft never stashes, resets, cleans or forces.",
                p.text_secondary,
            );
            ui.add_space(theme::SPACE_2);
            ui.horizontal(|ui| {
                let cancel = Pill::new(PillKind::Secondary, "Cancel").show(ui);
                // Once, when the step opens: asking every frame would take focus back from "Check out here" on Tab.
                if confirm.focus_pending.replace(false) {
                    cancel.request_focus();
                }
                if cancel.clicked() {
                    acts.push(LocalAct::CancelConfirm);
                }
                if Pill::new(PillKind::Primary, "Check out here").show(ui).clicked() {
                    acts.push(LocalAct::ConfirmHere);
                }
            });
        });
        ui.add_space(theme::SPACE_2);
    }

    /// The answer of the last choice, or that one is on its way. A live region: it arrives away from focus.
    fn answer(&self, ui: &mut Ui) {
        let said = match (&self.said, self.busy()) {
            (_, true) => Some((BannerKind::Info, "Working…".to_string())),
            (Some(Ok(news)), _) => Some((BannerKind::Info, news.clone())),
            (Some(Err(error)), _) => Some((BannerKind::Warn, error.clone())),
            _ => None,
        };
        if let Some((kind, text)) = said {
            ui.add_space(theme::SPACE_2);
            let banner = components::banner(ui, kind, &text, "");
            ui.ctx().accesskit_node_builder(banner.id, |node| node.set_live(egui::accesskit::Live::Polite));
        }
    }

    /// The popover for the render tests, with this status already read.
    #[cfg(test)]
    pub(crate) fn fixture(status: Status, said: Option<Result<String, String>>) -> Self {
        Local { open: true, status: Fetch::Ready(status), work: Fetch::Idle, confirm: None, said, trigger: Cell::new(None) }
    }

    /// The confirm step open on the clone and branch of `status`, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_confirm(mut self) -> Self {
        self.ask_here();
        self
    }
}

/// Why the file choices are off, as the line under the first of them.
fn editor_why(checked_out: bool, target: Option<&Target>) -> &'static str {
    match (checked_out, target) {
        (false, _) => "check the change out first",
        (true, None) => "select a changed line first",
        (true, Some(_)) => "",
    }
}

/// What a reused worktree adds to its news: the change moved on since it was made.
fn other_commit_words(other: bool) -> &'static str {
    if other { " It is at a different commit than the PR head. PR Craft does not move it; update it with git in that folder." } else { "" }
}

#[cfg(test)]
mod tests {
    use pr_craft_core::Forge;
    use pr_craft_core::checkout::{Action, LocalState};
    use pr_craft_engine::local::CloneStatus;

    use super::*;

    fn change() -> ChangeRef {
        ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 7, host: None }
    }

    fn status(here: Action, editor_root: Option<&str>) -> Status {
        let state = LocalState { branch: Some("main".into()), ..LocalState::default() };
        Status {
            change: change(),
            checkouts_root: Some("/c".into()),
            clone: Some(CloneStatus { path: "/work/api".into(), remote: Some("origin".into()), state: Some(state), error: None }),
            worktree: None,
            worktree_other_commit: false,
            worktree_path: Some("/c/acme-api-7".into()),
            head_branch: Some("feature".into()),
            editor_root: editor_root.map(str::to_string),
            worktree_action: Action::yes(),
            here_action: here,
            forge_error: None,
            setup: None,
        }
    }

    #[test]
    fn check_out_here_asks_first_naming_the_clone_and_the_branch_and_a_refusal_never_asks() {
        let ctx = Context::default();
        let mut local = Local::fixture(status(Action::yes(), None), None);
        local.act(&ctx, &change(), LocalAct::AskHere, None);
        let asked = local.confirm.as_ref().expect("the confirm step opened");
        assert_eq!((asked.folder.as_str(), asked.from.as_str(), asked.branch.as_str()), ("/work/api", "main", "feature"));
        assert!(!local.busy(), "nothing ran before the answer");
        local.act(&ctx, &change(), LocalAct::CancelConfirm, None);
        assert!(!local.confirming());

        let mut refused = Local::fixture(status(Action::no("The clone has uncommitted changes (a.rs)."), None), None);
        refused.act(&ctx, &change(), LocalAct::AskHere, None);
        assert!(!refused.confirming() && !refused.busy());
        assert!(matches!(&refused.said, Some(Err(why)) if why.contains("uncommitted")), "the engine's reason is shown as it is");
    }

    #[test]
    fn an_editor_needs_a_checkout_and_a_selected_line_and_says_which_is_missing() {
        let ctx = Context::default();
        let target = Target { path: "src/main.rs".into(), line: 3 };
        let mut none = Local::fixture(status(Action::yes(), None), None);
        none.act(&ctx, &change(), LocalAct::Editor(Editor::VsCode), Some(&target));
        assert!(!none.busy() && matches!(&none.said, Some(Err(why)) if why.contains("not checked out")));
        let mut no_line = Local::fixture(status(Action::yes(), Some("/c/acme-api-7")), None);
        no_line.act(&ctx, &change(), LocalAct::Editor(Editor::Zed), None);
        assert!(!no_line.busy() && matches!(&no_line.said, Some(Err(why)) if why.contains("Select a changed line")));
    }

    #[test]
    fn remove_the_worktree_is_named_for_a_finished_change_with_a_worktree_and_absent_for_an_open_one() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::{By, NodeT, Queryable};
        let named = |worktree: Option<&str>, finished: bool| {
            let mut made = status(Action::yes(), None);
            made.worktree = worktree.map(str::to_string);
            let local = Local::fixture(made, None);
            let mut ready = false;
            let mut harness = Harness::builder().with_size(egui::vec2(600.0, 500.0)).build_ui(move |ui| {
                if !ready {
                    crate::prefs::install_defaults(ui.ctx());
                    let look = theme::Look::default();
                    theme::install(ui.ctx(), look);
                    theme::apply_style(ui.ctx(), &look);
                    crate::fonts::install_blocking(ui.ctx(), &pr_craft_core::settings::Settings::default());
                    ready = true;
                }
                local.popover(ui.ctx(), &change(), None, finished, &mut Vec::new());
            });
            harness.run();
            harness
                .query_all(By::new().role(egui::accesskit::Role::Button))
                .any(|n| n.accesskit_node().label().is_some_and(|name| name.starts_with("Remove the worktree. /c/acme-api-7")))
        };
        assert!(named(Some("/c/acme-api-7"), true), "merged or closed, with a worktree");
        assert!(!named(Some("/c/acme-api-7"), false), "an open change keeps it quiet");
        assert!(!named(None, true), "nothing to remove");
    }
}
