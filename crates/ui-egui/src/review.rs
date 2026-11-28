//! The review screen: files, the diff, and the review being written (Figma frames 02, 04, 05, 10, 23, 33, 34, 46).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use chrono::{DateTime, Utc};
use craft::egui::{self, Align, Context, FontId, Layout, Margin, OpenUrl, Rect, RichText, ScrollArea, TextEdit, Ui, UiBuilder};
use pr_craft_core::ChangeRef;
use pr_craft_core::commits as pick;
use pr_craft_core::context::{self, Gap, Kind, Remaining, Reveal};
use pr_craft_core::diff::{DiffLine, FileDiff, FileStatus, Hunk};
use pr_craft_core::diff_view::{self, LayoutOptions};
use pr_craft_core::editor::Editor;
use pr_craft_core::filter;
use pr_craft_core::review::{Draft, LineRef, LocalReview, Verdict};
use pr_craft_core::run::{AiDraft, DraftState};
use pr_craft_core::stack::StackLayer;
use pr_craft_core::thread::ReviewThread;
use pr_craft_core::view::{self as file_view, FileView, MdBlockView};
use pr_craft_engine::commits as commits_engine;
use pr_craft_engine::review::{self as engine, NewDraft, ReviewView};
use pr_craft_engine::stack::{self as stacks, WholeStack};
use pr_craft_engine::thread::{self as thread_engine, ThreadList};
use pr_craft_engine::turn::{self, HandOff, SinceLast};
use pr_craft_engine::view as engine_view;

use crate::activity::{self, PanelAct};
use crate::ai::{self, Fetch, Runs};
use crate::commits::{CommitAct, Commits};
use crate::components::{self, BannerKind, Bar, BarButton, CardKind, Cell, CodeStyle, Gutter, Half, Pill, PillKind, TagKind};
use crate::keymap;
use crate::local::{Local, LocalAct, Target};
use crate::menu;
use crate::share::{self, ShareSheet};
use crate::sheet::{self, AiSheet};
use crate::theme;
use crate::threads::{self, ThreadAct};
use crate::views::{self, Pressed, Views};

/// Files with more changed lines than this start folded, so a large change opens quickly (frame 33).
const LARGE_FILE_LINES: u32 = 400;

/// The room the Unified/Split switch and the Diff options control take at the right of the diff header.
const LAYOUT_CONTROLS: f32 = 280.0;

/// Said when another review would replace one whose post or hand-off is still on its way.
pub(crate) const WAIT_FOR_FORGE: &str = "Wait for the forge to answer before leaving this review.";

pub enum Outcome {
    Stay,
    Back,
    /// Open this layer of the stack on its own diff.
    Open(ChangeRef, String),
    /// "Create an agent" on the AI review sheet: open Settings on a new agent.
    NewAgent,
}
/// Back and the trail of a change on the left of the title bar. True when Back was clicked.
fn back_trail(ui: &mut Ui, bar: &Bar, change: &ChangeRef) -> bool {
    let key = keymap::label(ui, "review.back");
    let place = format!("{}{}", change.forge.number_sigil(), change.number);
    // The change number is always whole: when the strip is short, "PR Craft" goes first, then the repository.
    let room = bar.room();
    let width = |brand: bool, repo: bool| {
        let (gap, sep) = (8.0, components::text_width(ui, "/", 13.0));
        let back = components::text_width(ui, "‹ Inbox", 12.5) + 20.0;
        let mut total = back + gap + 14.0;
        let mut after_first = false;
        let mut add = |text_width: f32| {
            total += gap + if after_first { sep + gap } else { 0.0 } + text_width;
            after_first = true;
        };
        if brand {
            add(components::text_width(ui, "PR Craft", 13.0));
        }
        if repo {
            add(components::text_width(ui, &change.repo, 13.0));
        }
        add(components::text_width(ui, &place, 13.0));
        total
    };
    let (brand, repo) = [(true, true), (false, true), (false, false)].into_iter().find(|&(b, r)| width(b, r) <= room).unwrap_or((false, false));
    bar.left(ui, |ui| {
        let back = components::title_button(ui, "‹ Inbox", &key).clicked();
        let trail: Vec<&str> = if repo { vec![change.repo.as_str(), place.as_str()] } else { vec![place.as_str()] };
        components::breadcrumb_with(ui, brand, &trail);
        back
    })
}

/// What a verdict means, under its name.
fn verdict_note(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Comment => "Feedback without a verdict",
        Verdict::Approve => "Looks good to merge",
        Verdict::RequestChanges => "Blocks merge until resolved",
    }
}

pub struct ReviewScreen {
    change: ChangeRef,
    title: String,
    state: State,
    /// Opened from Back to you, so the "Since your review" view is offered.
    from_back: bool,
    /// Leave on the next frame: the command palette asked for Back.
    leave: bool,
}

enum State {
    Loading(Receiver<Result<ReviewView, String>>),
    Failed(String),
    Ready(Box<Ready>),
}

impl ReviewScreen {
    /// Starts `review.open` on its own thread: the CLIs wait on the network.
    pub fn open(ctx: &Context, change: ChangeRef, title: String, from_back: bool) -> Self {
        ReviewScreen { state: State::Loading(load(ctx, change.clone())), change, title, from_back, leave: false }
    }

    /// Collects the answer of a post or a hand-off. The app calls it every frame, also while Settings or Activity covers
    /// the review, so `busy` clears when the forge has answered.
    pub(crate) fn poll(&mut self, ctx: &Context) {
        if let State::Ready(ready) = &mut self.state {
            ready.poll_submit();
            ready.poll_handoff();
            ready.poll_write(ctx);
        }
    }
    /// The palette closed: the review was not being typed in, whatever its fields held before.
    pub(crate) fn forget_typing(&mut self) {
        if let State::Ready(ready) = &mut self.state {
            ready.was_typing = false;
        }
    }

    /// "Back to the inbox" from the palette while a comment, a draft edit or a hand-off is open: Esc would throw that text away, so
    /// the review stays and says what to do. Returns true when it stayed.
    pub(crate) fn back_blocked(&mut self, ctx: &Context) -> bool {
        let State::Ready(ready) = &mut self.state else { return false };
        let open = (ready.composer.is_some() && !ready.readonly()) || ready.editing.is_some() || ready.handoff.is_some();
        if open {
            ready.notice = Some(format!(
                "A comment or hand-off is open. Save it with {} or cancel it with {} first; going back would discard it.",
                keymap::label(ctx, "review.submit"),
                keymap::label(ctx, "review.back")
            ));
        }
        open
    }

    /// "Back to the inbox" from the palette: leaves the review, not the Esc chain (which would first clear a selection or leave a
    /// comparison view). It waits like Back does while a post is on its way, and stays when it would discard a comment.
    pub(crate) fn request_back(&mut self, ctx: &Context) {
        if self.back_blocked(ctx) {
            return;
        }
        if self.busy() {
            if let State::Ready(ready) = &mut self.state {
                ready.notice = Some(WAIT_FOR_FORGE.into());
            }
            return;
        }
        self.leave = true;
        ctx.request_repaint();
    }

    /// What this review can do now, for the command palette to offer only the keys that would act.
    pub(crate) fn facts(&self, runs: &Runs) -> pr_craft_core::palette::ReviewFacts {
        let State::Ready(r) = &self.state else { return pr_craft_core::palette::ReviewFacts { from_back: self.from_back, ..Default::default() } };
        pr_craft_core::palette::ReviewFacts {
            from_back: r.from_back,
            readonly: r.readonly(),
            ai_drafts: !r.ai_pending.is_empty(),
            stacked: !r.layers().is_empty(),
            threads: matches!(&r.threads, Fetch::Ready(list) if !list.threads.is_empty()),
            can_submit: self.submit_offered(),
            run_detail_open: r.run_detail.is_some(),
            drafts: !r.view.local.drafts.is_empty(),
            file_offers_views: r.current_file().is_some_and(|f| views::offered(f).len() > 1),
            diff_drawn: r.current_file().is_some_and(|f| r.expanded(f) && !r.drawn_rendered(f) && !f.hunks.is_empty()),
            file_offers_images: r.current_file().is_some_and(|f| {
                views::offered(f)
                    .iter()
                    .any(|v| matches!(v, FileView::ImageTwoUp | FileView::ImageSwipe | FileView::ImageOnion | FileView::ImageDifference))
            }),
            file_current: r.current_file().is_some(),
            file_in_folder: r.current_file().is_some_and(|f| !parent_dir(&f.path).is_empty()),
            runs_going: runs.of(&r.view.change).any(|run| run.state.is_active() && !run.stopping),
        }
    }

    /// Submit would submit: no hand-off, comment box or draft edit is open for ⌘↵ to send or save instead, and no post is on its way.
    pub(crate) fn submit_offered(&self) -> bool {
        matches!(&self.state, State::Ready(r) if r.handoff.is_none() && r.composer.is_none() && r.editing.is_none() && r.submitting.is_none())
    }

    /// Submits the review now, as the Submit review button does, from outside the screen (the command palette).
    pub(crate) fn submit_review(&mut self, ctx: &Context) {
        if !self.submit_offered() {
            return;
        }
        if let State::Ready(ready) = &mut self.state
            && let Err(error) = ready.submit(ctx)
        {
            ready.error = Some(crate::fail(error));
        }
    }

    /// The change this screen shows and its title, for actions that act on the open review from outside it.
    pub(crate) fn subject(&self) -> (&ChangeRef, &str) {
        (&self.change, &self.title)
    }

    /// A post or a hand-off is on its way: leaving this review now would lose its answer.
    pub(crate) fn busy(&self) -> bool {
        matches!(&self.state, State::Ready(ready) if ready.submitting.is_some() || ready.handing.is_some() || ready.writing.is_some())
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, runs: &mut Runs, bar: &mut Bar) -> Outcome {
        if std::mem::take(&mut self.leave) {
            return Outcome::Back;
        }
        if let State::Loading(rx) = &self.state {
            match rx.try_recv() {
                Ok(Ok(view)) => self.state = State::Ready(Box::new(Ready::new(ui.ctx(), view, self.from_back))),
                Ok(Err(error)) => self.state = State::Failed(error),
                Err(TryRecvError::Disconnected) => self.state = State::Failed("loading the change stopped".into()),
                Err(TryRecvError::Empty) => {}
            }
        }
        let p = theme::pal(ui);
        match &mut self.state {
            State::Loading(_) => {
                let leave = back_trail(ui, bar, &self.change) || keymap::pressed(ui, "review.back");
                components::padded(ui, Margin::same(24), |ui| {
                    components::label(ui, self.title.as_str(), 18.0, p.graphite);
                    components::note_sized(ui, format!("Fetching the files of {}…", self.change.reference()), 13.0, p.text_secondary);
                });
                if leave { Outcome::Back } else { Outcome::Stay }
            }
            State::Failed(error) => {
                let leave = back_trail(ui, bar, &self.change) || keymap::pressed(ui, "review.back");
                let mut retry = false;
                components::padded(ui, Margin::same(24), |ui| {
                    components::label(ui, self.title.as_str(), 18.0, p.graphite);
                    components::note_sized(ui, format!("Could not open {}: {error}", self.change.reference()), 13.0, p.rose_text);
                    ui.add_space(theme::SPACE_2);
                    retry = Pill::new(PillKind::Secondary, "Retry").show(ui).clicked();
                });
                if retry {
                    self.state = State::Loading(load(ui.ctx(), self.change.clone()));
                }
                if leave { Outcome::Back } else { Outcome::Stay }
            }
            State::Ready(ready) => ready.ui(ui, runs, bar),
        }
    }

    /// The same review with the Share sheet open, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_share(mut self) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.share = Some(ShareSheet::open());
        }
        self
    }

    /// The same review with the AI review sheet open, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_sheet(mut self, sheet: AiSheet) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.sheet = Some(sheet);
        }
        self
    }

    /// The same review with a comment box open on the first line of the first hunk holding `text`, for the tests.
    #[cfg(test)]
    pub(crate) fn with_composer(mut self, text: &str) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.selection = Some(Selection { file: 0, hunk: 0, anchor: 0, head: 0 });
            ready.composer = Some(text.to_string());
        }
        self
    }

    /// How many threads T steps through, for the tests.
    #[cfg(test)]
    pub(crate) fn thread_stops(&self) -> usize {
        match &self.state {
            State::Ready(ready) => ready.thread_order().len(),
            _ => 0,
        }
    }

    /// The same review with `path` marked viewed (so folded), for the tests.
    #[cfg(test)]
    pub(crate) fn with_viewed(mut self, path: &str) -> Self {
        if let State::Ready(ready) = &mut self.state
            && let Some(hash) = ready.view.files.iter().find(|f| f.path == path).map(|f| f.hash)
        {
            ready.view.local.viewed.insert(path.to_string(), hash);
        }
        self
    }

    /// The same review with the first line of the first hunk selected and no comment box, for the tests.
    #[cfg(test)]
    pub(crate) fn with_selected_line(mut self) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.selection = Some(Selection { file: 0, hunk: 0, anchor: 0, head: 0 });
        }
        self
    }

    /// The same review with lines `lo` to `hi` of hunk `hunk` of the first file selected, for the tests.
    #[cfg(test)]
    pub(crate) fn with_lines(mut self, hunk: usize, lo: usize, hi: usize) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.selection = Some(Selection { file: 0, hunk, anchor: lo, head: hi });
        }
        self
    }

    /// The same review with file `file` current (the selection stays where it was), for the tests.
    #[cfg(test)]
    pub(crate) fn with_current_file(mut self, file: usize) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.file = file;
        }
        self
    }

    /// The same review with `path` already read as an SVG, `old` before and `new` after, for the tests.
    #[cfg(test)]
    pub(crate) fn with_svg(mut self, path: &str, old: &str, new: &str) -> Self {
        if let State::Ready(ready) = &mut self.state
            && let Some(file) = ready.view.files.iter().find(|f| f.path == path)
        {
            ready.views.preload_svg(file, old, new);
        }
        self
    }

    /// The same review with no draft on this Mac, for the tests.
    #[cfg(test)]
    pub(crate) fn with_no_drafts(mut self) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.view.local.drafts.clear();
        }
        self
    }

    /// The same review with every file marked viewed, for the tests.
    #[cfg(test)]
    pub(crate) fn with_every_file_viewed(mut self) -> Self {
        if let State::Ready(ready) = &mut self.state {
            let all: Vec<(String, u64)> = ready.view.files.iter().map(|f| (f.path.clone(), f.hash)).collect();
            ready.view.local.viewed.extend(all);
        }
        self
    }

    /// The files marked viewed, the folders folded and the Files filter, for the tests.
    #[cfg(test)]
    pub(crate) fn probe(&self) -> (usize, Vec<String>, String) {
        match &self.state {
            State::Ready(ready) => {
                let mut folded: Vec<String> = ready.folded.iter().cloned().collect();
                folded.sort();
                (ready.view.local.viewed.len(), folded, ready.filter_text.clone())
            }
            _ => (0, Vec::new(), String::new()),
        }
    }

    /// The file the review is on, the lines selected (text and count), whether the Open locally popover is open and the viewed marks
    /// a folder would change, for the tests.
    #[cfg(test)]
    pub(crate) fn probe_more(&self, dir: &str) -> Probe {
        match &self.state {
            State::Ready(r) => {
                let (want, changing) = r.folder_marks(dir);
                (r.file, r.selected_lines(), r.local.open, (want, changing.into_iter().map(|(path, _)| path).collect()))
            }
            _ => (0, None, false, (false, Vec::new())),
        }
    }

    /// The text of the open comment box, for the tests.
    #[cfg(test)]
    pub(crate) fn composer_text(&self) -> Option<&str> {
        match &self.state {
            State::Ready(ready) => ready.composer.as_deref(),
            _ => None,
        }
    }

    /// The same review with these diff options, and the options popover open or not, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_layout(mut self, layout: LayoutOptions, options_open: bool) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.layout = layout;
            ready.options_open = options_open;
        }
        self
    }

    /// The same review with its commits read and the picker open, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_commits(mut self, list: pr_craft_engine::commits::CommitList, picked: Option<pick::Picked>) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.commits.list = Fetch::Ready(list);
            ready.commits.picked = picked;
            ready.commits.open = true;
        }
        self
    }

    /// The same review with the Open locally popover as `local` has it, and the first line of the first hunk selected so the
    /// editors have a place to open, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_local(mut self, local: Local) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.selection = Some(Selection { file: 0, hunk: 0, anchor: 0, head: 0 });
            ready.local = local;
        }
        self
    }

    /// The same review with these threads read from the forge, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_threads(mut self, list: ThreadList) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.threads = Fetch::Ready(list);
        }
        self
    }

    /// The same review with the "since your review" comparison loaded, for the render tests.
    #[cfg(test)]
    pub(crate) fn with_since(mut self, since: SinceLast) -> Self {
        if let State::Ready(ready) = &mut self.state {
            ready.since = Fetch::Ready(since);
        }
        self
    }

    /// A review of `view` as the screen builds it once the forge has answered, with nothing read from this Mac and no thread
    /// started, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(view: ReviewView, layers: Vec<StackLayer>, from_back: bool) -> Self {
        let (change, title) = (view.change.clone(), view.meta.title.clone());
        let ready = Ready::with(view, from_back, BTreeMap::new(), None, Fetch::Ready(layers));
        ReviewScreen { change, title, state: State::Ready(Box::new(ready)), from_back, leave: false }
    }
}

fn load(ctx: &Context, change: ChangeRef) -> Receiver<Result<ReviewView, String>> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // The screen may have closed first; nobody is left to tell.
        let _ = tx.send(engine::open(&change).map_err(crate::fail));
        ctx.request_repaint();
    });
    rx
}

/// Selected lines, always inside one hunk: GitHub anchors a multi-line comment within one hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    file: usize,
    hunk: usize,
    anchor: usize,
    head: usize,
}

impl Selection {
    fn lines(self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    fn contains(self, file: usize, hunk: usize, line: usize) -> bool {
        let (lo, hi) = self.lines();
        self.file == file && self.hunk == hunk && (lo..=hi).contains(&line)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    None,
    File,
    Hunk,
}

enum Act {
    Select {
        file: usize,
        hunk: usize,
        line: usize,
        extend: bool,
    },
    GoFile(usize),
    Step {
        hunks: bool,
        forward: bool,
    },
    StepDraft {
        forward: bool,
    },
    ToggleViewed(usize),
    Unfold(String),
    OpenComposer,
    SaveComposer,
    CancelComposer,
    StartEdit(u64),
    /// `E`: edit the draft `[` `]` last moved to, else the first draft of the current file.
    EditHere,
    SaveEdit,
    CancelEdit,
    DeleteDraft(u64),
    FilterChanged,
    SaveFilter,
    Verdict(Verdict),
    SaveBody,
    Submit,
    /// After a post that ran out of time: the reader checked the forge and says post again.
    ConfirmPost,
    DismissPost,
    CopyMarkdown,
    OpenOnForge,
    /// Open this URL, as the forge shows something PR Craft cannot.
    OpenUrl(String),
    Back,
    ToggleWhole,
    ToggleSince,
    OpenHandOff,
    CancelHandOff,
    SendHandOff,
    OpenLayer(ChangeRef, String),
    SetView(usize, FileView),
    ToggleView(usize),
    StepBlock {
        forward: bool,
    },
    CommentBlock {
        file: usize,
        item: usize,
    },
    OpenSheet,
    OpenShare,
    StopRuns,
    Details(String),
    /// Keep an AI draft as a draft of the review; `None` is the current one. With `edit`, open it for editing.
    KeepAi {
        id: Option<String>,
        edit: bool,
    },
    DiscardAi(Option<String>),
    StepAi {
        forward: bool,
    },
    /// Make this AI draft the one A, ⇧A and X act on.
    FocusAi(String),
    /// Save these diff options for this repository and draw with them.
    SetLayout(LayoutOptions),
    ToggleOptions,
    /// Read more of a gap of file `file`.
    Expand {
        file: usize,
        gap: usize,
        kind: Kind,
    },
    /// Read more around the current hunk: the keys.
    ExpandAround(Kind),
    Thread(ThreadAct),
    ThreadStep {
        forward: bool,
    },
    /// ⇧R: resolve or reopen the current thread.
    ThreadResolveCurrent,
    /// ⌥R: focus the current thread's reply field.
    ThreadReplyCurrent,
    Commit(CommitAct),
    Local(LocalAct),
    /// Open the menu of line `line`, or of the selection when the line is inside it.
    LinesMenu {
        file: usize,
        hunk: usize,
        line: usize,
        at: egui::Pos2,
        restore: Option<egui::Id>,
    },
    FolderMenu {
        dir: String,
        at: egui::Pos2,
        restore: Option<egui::Id>,
    },
    FoldFolder(String),
    UnfoldFolder(String),
    FoldAll,
    /// Mark every visible file of the folder viewed, or all of them unviewed when they all are.
    ViewedFolder(String),
    CopyText {
        text: String,
        said: String,
    },
    /// ⌘C: the selected lines to the clipboard.
    CopyLines,
    /// The Files filter takes this text, as if typed.
    FilterTo(String),
}

/// Closes the comment box of a rendered block.
fn drop_block(block: &mut Option<BlockTarget>, composer: &mut Option<String>) {
    if block.take().is_some() {
        *composer = None;
    }
}

/// Closes a block's comment box when it no longer belongs on screen: another file is current, or its file is not drawn rendered.
fn reconcile_block(block: &mut Option<BlockTarget>, composer: &mut Option<String>, file: usize, rendered_here: bool) {
    if block.is_some_and(|b| b.file != file || !rendered_here) {
        drop_block(block, composer);
    }
}

/// Makes `to` the current file, at its first hunk, when another file was.
fn make_current(file: &mut usize, hunk: &mut usize, to: usize) {
    if *file != to {
        (*file, *hunk) = (to, 0);
    }
}

/// A comment being written on a rendered Markdown block: the file, the block and the source line it posts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockTarget {
    file: usize,
    item: usize,
    line: LineRef,
}

struct Ready {
    view: ReviewView,
    filter_text: String,
    patterns: Vec<String>,
    /// Indices into `view.files` that the filter leaves visible.
    visible: Vec<usize>,
    file: usize,
    hunk: usize,
    focus: Focus,
    selection: Option<Selection>,
    composer: Option<String>,
    focus_composer: bool,
    editing: Option<(u64, String)>,
    unfolded: HashSet<String>,
    body: String,
    submitting: Option<Receiver<Result<LocalReview, String>>>,
    /// Asking whether to post again, because the last post ran out of time and may have gone through.
    confirm_post: bool,
    error: Option<String>,
    notice: Option<String>,
    /// The draft `[` and `]` last moved to.
    draft_focus: Option<u64>,
    /// Focus `draft_focus` on this frame only, so Tab can leave it afterwards.
    focus_draft: bool,
    /// Drafts drawn under their line this frame; the rest are edited from the review panel.
    inline: HashSet<u64>,
    /// A text field had focus last frame. egui drops focus on Esc before this frame runs, so Esc
    /// is judged by this, never by the current focus.
    was_typing: bool,
    /// The stack this change sits in, read after `review.open`. Shown as a strip when it has two layers or more.
    layers: Fetch<Vec<StackLayer>>,
    /// The whole-stack view (`S`); `Idle` while the single layer shows.
    whole: Fetch<WholeStack>,
    /// The file and hunk J K N P last moved to in a read-only view (whole stack or since your review).
    readonly_at: (usize, usize),
    /// Opened from Back to you, so `R` offers the since-your-review view.
    from_back: bool,
    /// What changed since the user's last review (`R`); `Idle` while all changes show.
    since: Fetch<SinceLast>,
    /// The hand-off sheet (`H`): the login typed so far.
    handoff: Option<String>,
    /// Focus the sheet's field on this frame only.
    focus_handoff: bool,
    handing: Option<Receiver<Result<HandOff, String>>>,
    /// File views: remembered choices, what each loaded and where the reader is in each.
    views: Views,
    block: Option<BlockTarget>,
    /// The AI review sheet (⌥⌘A), while it is open.
    sheet: Option<AiSheet>,
    /// The Share sheet (⇧⌘I), while it is open.
    share: Option<ShareSheet>,
    /// The run whose details window is open, and the node shown in it.
    run_detail: Option<(String, Option<u32>)>,
    /// The pending AI drafts of this change this frame, by file and line.
    ai_pending: Vec<ai::Pending>,
    /// The AI draft `⌥↓` and `⌥↑` last moved to; A, ⇧A and X act on it.
    ai_focus: Option<String>,
    /// Focus `ai_focus`'s Keep button on this frame only, so Tab can leave it afterwards.
    focus_ai: bool,
    /// AI drafts drawn under their line this frame; the rest are listed in the panel.
    ai_inline: HashSet<String>,
    /// The diff options in force: this repository's saved ones.
    layout: LayoutOptions,
    /// The window's width this frame, for the split view's minimum.
    window: f32,
    options_open: bool,
    /// Where the Diff options control was last drawn, so a press on it is not "outside" the popover.
    options_trigger: Option<Rect>,
    /// What the reader revealed in the gaps of each file, by file index; one entry per gap.
    reveals: HashMap<usize, Vec<Reveal>>,
    /// The read of lines for a gap that is on its way, and the reads queued after it.
    expanding: Option<Expanding>,
    expand_queue: VecDeque<(usize, usize, Kind)>,
    /// The review threads on the forge, read after `review.open`.
    threads: Fetch<ThreadList>,
    cards: threads::Cards,
    /// A reply or a resolve on its way to the forge.
    writing: Option<ThreadWrite>,
    /// The commit picker: the list, the pick, and the diff of the picked commits.
    commits: Commits,
    /// The Open locally popover (⌘O).
    local: Local,
    /// Folders of the Files list that are folded.
    folded: HashSet<String>,
    /// "Show viewed files" was pressed on the all-viewed state: the files list below it shows again.
    show_viewed: bool,
    /// Focus the Submit button on this frame only ("Go to Submit").
    focus_submit: bool,
}

impl Ready {
    fn new(ctx: &Context, view: ReviewView, from_back: bool) -> Self {
        let change = view.change.clone();
        let (remembered, error) = match engine_view::views() {
            Ok(saved) => (saved, None),
            Err(e) => (BTreeMap::new(), Some(format!("Could not read your saved views: {}", crate::fail(e)))),
        };
        let layers = Fetch::start(ctx, move || stacks::list(&change).map_err(crate::fail));
        let for_threads = view.change.clone();
        let mut ready = Self::with(view, from_back, remembered, error, layers);
        ready.threads = Fetch::start(ctx, move || thread_engine::list(&for_threads).map_err(crate::fail));
        ready
    }

    fn with(
        view: ReviewView,
        from_back: bool,
        remembered: BTreeMap<String, FileView>,
        error: Option<String>,
        layers: Fetch<Vec<StackLayer>>,
    ) -> Self {
        let filter_text = view.filter.join(" ");
        let body = view.local.body.clone();
        let layout = view.layout.clone();
        let file = 0;
        let mut ready = Ready {
            view,
            filter_text,
            patterns: Vec::new(),
            visible: Vec::new(),
            file,
            hunk: 0,
            focus: Focus::None,
            selection: None,
            composer: None,
            focus_composer: false,
            editing: None,
            unfolded: HashSet::new(),
            body,
            submitting: None,
            confirm_post: false,
            error: None,
            notice: None,
            draft_focus: None,
            focus_draft: false,
            was_typing: false,
            layers,
            whole: Fetch::Idle,
            readonly_at: (0, 0),
            from_back,
            since: Fetch::Idle,
            handoff: None,
            focus_handoff: false,
            handing: None,
            inline: HashSet::new(),
            views: Views::new(remembered),
            block: None,
            sheet: None,
            share: None,
            run_detail: None,
            ai_pending: Vec::new(),
            ai_focus: None,
            focus_ai: false,
            ai_inline: HashSet::new(),
            layout,
            window: theme::WIDE_WINDOW,
            options_open: false,
            options_trigger: None,
            reveals: HashMap::new(),
            expanding: None,
            expand_queue: VecDeque::new(),
            threads: Fetch::Idle,
            cards: threads::Cards::default(),
            writing: None,
            commits: Commits::new(),
            local: Local::new(),
            folded: HashSet::new(),
            show_viewed: false,
            focus_submit: false,
        };
        ready.error = error;
        ready.refilter();
        ready
    }

    fn refilter(&mut self) {
        self.patterns = self.filter_text.split([' ', ',']).filter(|p| !p.is_empty()).map(str::to_string).collect();
        self.visible = (0..self.view.files.len()).filter(|&i| filter::visible(&self.patterns, &self.view.files[i].path)).collect();
        if !self.visible.contains(&self.file) {
            self.file = self.visible.first().copied().unwrap_or(0);
            self.hunk = 0;
        }
    }

    fn viewed(&self, file: &FileDiff) -> bool {
        self.view.local.viewed.get(&file.path) == Some(&file.hash)
    }

    /// Hunks are drawn only for files that are not folded.
    fn expanded(&self, file: &FileDiff) -> bool {
        shows_hunks(file, self.viewed(file), &self.unfolded)
    }

    fn ui(&mut self, ui: &mut Ui, runs: &mut Runs, bar: &mut Bar) -> Outcome {
        self.poll_submit();
        self.layers.poll();
        self.whole.poll();
        self.since.poll();
        self.poll_handoff();
        self.window = ui.available_rect_before_wrap().width();
        self.poll_expand(ui.ctx());
        self.threads.poll();
        self.drop_stale_thread();
        if !matches!(self.threads, Fetch::Ready(_)) {
            self.cards.typing = None;
        }
        self.commits.list.poll();
        self.commits.range.poll();
        self.poll_write(ui.ctx());
        self.local.poll(ui.ctx(), &self.view.change);
        self.views.tidy(&self.view.files, self.file);
        let rendered_here = self.rendered_current();
        reconcile_block(&mut self.block, &mut self.composer, self.file, rendered_here);
        self.inline.clear();
        self.ai_inline.clear();
        self.ai_pending = ai::pending(runs.of(&self.view.change), &self.view.anchor);
        if let Some(outcome) = self.sheet_ui(ui.ctx(), runs) {
            return outcome;
        }
        self.share_ui(ui.ctx());
        self.detail_ui(ui.ctx(), runs);
        let mut acts = Vec::new();
        self.keys(ui, &mut acts);
        self.menu_acts(ui.ctx(), &mut acts);
        self.title_bar(ui, bar, &mut acts);
        let p = theme::pal(ui);
        let rect = ui.available_rect_before_wrap();
        let (files_width, panel_width) =
            if rect.width() < theme::WIDE_WINDOW { (theme::FILES_NARROW, theme::PANEL_NARROW) } else { (theme::FILES_WIDTH, theme::PANEL_WIDTH) };
        let files = Rect::from_min_size(rect.min, egui::vec2(files_width + 1.0, rect.height()));
        let panel = Rect::from_min_max(egui::pos2(rect.right() - panel_width - 1.0, rect.top()), rect.right_bottom());
        let diff = Rect::from_min_max(files.right_top(), panel.left_bottom());
        components::pane_at(ui, "files", files, p.hover, Margin { left: 8, right: 9, top: 12, bottom: 8 }, |ui| self.files_pane(ui, &mut acts));
        components::seam_right(ui, files);
        components::pane_at(ui, "diff", diff, p.panel, Margin::ZERO, |ui| self.diff_pane(ui, diff.width(), &mut acts));
        components::seam_right(ui, diff);
        components::pane_at(ui, "review", panel, p.hover, Margin { left: 16, right: 16, top: 14, bottom: 12 }, |ui| {
            self.review_pane(ui, &mut acts, runs);
        });
        self.options_popover(ui.ctx(), diff, &mut acts);
        self.commits_popover(ui.ctx(), &mut acts);
        self.local_popover(ui.ctx(), &mut acts);
        self.was_typing = ui.ctx().text_edit_focused();
        if self.focus_draft {
            self.focus_draft = false;
        }
        self.focus_ai = false;
        self.cards.focus = None;
        self.apply(ui.ctx(), acts, runs)
    }

    /// The review's part of the title bar: back and the trail on the left, the review's actions on the right.
    fn title_bar(&self, ui: &mut Ui, bar: &mut Bar, acts: &mut Vec<Act>) {
        let change = &self.view.change;
        bar.right(ui, |ui| {
            if components::title_action(ui, &format!("Open on {}", change.forge.name()), "").clicked() {
                acts.push(Act::OpenOnForge);
            }
            let open = components::title_action(ui, "Open", &keymap::label(ui, "review.open_locally"));
            self.local.trigger.set(Some(open.rect));
            if open.clicked() {
                acts.push(Act::Local(LocalAct::Toggle));
            }
            if components::title_action(ui, "Share image", &keymap::label(ui, "review.share")).clicked() {
                acts.push(Act::OpenShare);
            }
            if components::title_action(ui, "AI review", &keymap::label(ui, "review.ai_review")).clicked() {
                acts.push(Act::OpenSheet);
            }
            if self.from_back {
                let text = if self.since_shown() { "All changes" } else { "Since your review" };
                if components::title_action(ui, text, &keymap::label(ui, "review.since_review")).clicked() {
                    acts.push(Act::ToggleSince);
                }
            }
        });
        if back_trail(ui, bar, change) {
            acts.push(Act::Back);
        }
    }

    /// The AI review sheet draws over the review and takes its own keys; the review stays still beneath it.
    fn sheet_ui(&mut self, ctx: &Context, runs: &mut Runs) -> Option<Outcome> {
        let sheet = self.sheet.as_mut()?;
        match sheet.ui(ctx, runs) {
            sheet::Event::None => None,
            sheet::Event::Close => {
                self.sheet = None;
                None
            }
            sheet::Event::CreateAgent => {
                self.sheet = None;
                Some(Outcome::NewAgent)
            }
        }
    }

    /// The Share sheet draws over the review like the AI review sheet and takes its own keys.
    fn share_ui(&mut self, ctx: &Context) {
        let Some(sheet) = self.share.as_mut() else { return };
        let stacked = match &self.layers {
            Fetch::Ready(layers) => share::Stacked::Known(layers.as_slice()),
            Fetch::Failed(error) => share::Stacked::Failed(error.as_str()),
            Fetch::Idle | Fetch::Loading(_) => share::Stacked::Reading,
        };
        if matches!(sheet.ui(ctx, &self.view, &stacked), share::Event::Close) {
            self.share = None;
        }
    }

    fn detail_ui(&mut self, ctx: &Context, runs: &mut Runs) {
        let Some((id, node)) = &mut self.run_detail else { return };
        if !activity::detail_window(ctx, runs, id, node) {
            self.run_detail = None;
        }
    }

    /// The AI draft A, ⇧A and X act on: the one stepped to (⌥↓), else the one under the selected lines or in the current
    /// hunk, else the first draft read at the commit on screen. Never a draft of an older commit by default.
    fn ai_current(&self) -> Option<&ai::Pending> {
        let at = ai::pick_current(&self.ai_pending, self.ai_focus.as_deref(), self.ai_under_cursor())?;
        self.ai_pending.get(at)
    }

    /// The id of the placed AI draft on the selected lines, or else in the current hunk.
    fn ai_under_cursor(&self) -> Option<&str> {
        let file = self.view.files.get(self.file)?;
        let selected = self.selection.filter(|s| s.file == self.file);
        let hunk = file.hunks.get(selected.map_or(self.hunk, |s| s.hunk))?;
        let lines = match selected {
            Some(s) => {
                let (lo, hi) = s.lines();
                hunk.lines.get(lo..=hi)?
            }
            None => hunk.lines.as_slice(),
        };
        ai::draft_on(&self.ai_pending, &file.path, lines).map(|p| p.draft.id.as_str())
    }

    /// Keeps an AI draft as a draft of the review. It posts only with the rest of the review, on Submit.
    fn keep_ai(&mut self, runs: &mut Runs, id: Option<String>, edit: bool) {
        let Some(id) = id.or_else(|| self.ai_current().map(|p| p.draft.id.clone())) else { return };
        match ai::keep(&self.view.change, &id) {
            Ok(kept) => {
                runs.set_draft(&id, DraftState::Kept);
                runs.refresh_now();
                let body = kept.local.drafts.iter().find(|d| d.id == kept.draft).map(|d| d.body.clone());
                self.set_local(kept.local);
                if let (true, Some(body)) = (edit, body) {
                    self.editing = Some((kept.draft, body));
                    // The editor takes focus on the next frame, so typed letters never fire shortcuts.
                    self.draft_focus = Some(kept.draft);
                    self.focus_draft = true;
                }
                self.error = None;
                self.notice = Some("Kept as a draft on this Mac. It posts only when you submit.".into());
                self.ai_focus = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn discard_ai(&mut self, runs: &mut Runs, id: Option<String>) {
        let Some(id) = id.or_else(|| self.ai_current().map(|p| p.draft.id.clone())) else { return };
        match ai::discard(&self.view.change, &id) {
            Ok(()) => {
                runs.set_draft(&id, DraftState::Discarded);
                runs.refresh_now();
                self.error = None;
                self.notice = Some("Discarded. Nothing was posted.".into());
                self.ai_focus = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// ⌥↓ and ⌥↑: the next or previous pending AI draft becomes the one A and X act on, and its Keep button takes focus.
    fn step_ai(&mut self, forward: bool) {
        let Some(pending) = ai::step_pending(&self.ai_pending, self.ai_focus.as_deref(), forward).and_then(|i| self.ai_pending.get(i)) else {
            return;
        };
        let (id, path) = (pending.draft.id.clone(), pending.draft.finding.path.clone());
        if let Some(file) = self.view.files.iter().position(|f| f.path == path) {
            make_current(&mut self.file, &mut self.hunk, file);
        }
        self.ai_focus = Some(id);
        self.focus_ai = true;
    }

    /// The AI drafts, one row each like Your review. The one A, ⇧A and X act on opens under its row with Keep and Discard,
    /// unless it already sits under its line in the diff.
    fn ai_panel(&self, ui: &mut Ui, acts: &mut Vec<Act>) {
        if self.ai_pending.is_empty() {
            return;
        }
        let p = theme::pal(ui);
        ui.add_space(theme::SPACE_3);
        ui.horizontal(|ui| {
            components::label(ui, "AI drafts", 13.5, p.graphite);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                components::label(ui, format!("{} not posted", self.ai_pending.len()), 12.0, p.text_tertiary);
            });
        });
        ui.add_space(theme::SPACE_2);
        ui.spacing_mut().item_spacing.y = 0.0;
        let current = self.ai_current().map(|p| p.draft.id.clone());
        for pending in &self.ai_pending {
            let finding = &pending.draft.finding;
            let name = finding.path.rsplit('/').next().unwrap_or(&finding.path);
            let place = format!("{name}:{}", finding.line.shown());
            let inline = self.ai_inline.contains(&pending.draft.id);
            let is_current = current.as_deref() == Some(pending.draft.id.as_str());
            let row = components::draft_row(ui, finding.body.lines().next().unwrap_or_default(), &place);
            if row.clicked() {
                acts.push(Act::FocusAi(pending.draft.id.clone()));
            }
            if inline || !is_current {
                continue;
            }
            let note = if pending.placed {
                format!("{place} · not under a line in this view")
            } else {
                format!("{place} · read at an earlier commit than this diff, so its line may have moved")
            };
            ai_card(ui, None, &pending.draft, Some(note), true, self.focus_ai, acts);
        }
        ui.spacing_mut().item_spacing.y = 3.0;
    }

    /// ⌥⌘A opens the AI review sheet and ⌘. stops this change's runs; A keeps the AI draft, ⇧A keeps it and edits, X discards
    /// it, and ⌥↓ ⌥↑ move between them.
    fn ai_keys(&self, ui: &Ui, typing: bool, acts: &mut Vec<Act>) {
        if keymap::consume(ui, "review.ai_review") {
            acts.push(Act::OpenSheet);
        }
        if keymap::consume(ui, "review.stop_runs") {
            acts.push(Act::StopRuns);
        }
        if typing || self.readonly() {
            return;
        }
        // Keeping and discarding act once per press: a key held down must not walk through the drafts.
        for (id, act) in [
            ("review.keep_ai", Act::KeepAi { id: None, edit: false }),
            ("review.keep_ai_edit", Act::KeepAi { id: None, edit: true }),
            ("review.discard_ai", Act::DiscardAi(None)),
        ] {
            if keymap::fresh(ui, id) {
                acts.push(act);
            }
        }
        for (id, forward) in [("review.next_ai", true), ("review.prev_ai", false)] {
            if keymap::pressed(ui, id) {
                acts.push(Act::StepAi { forward });
            }
        }
    }

    /// The Open locally keys: Esc closes the popover (or the confirm step in it); ⌘O opens it, or with it open opens VS Code;
    /// ⌘⇧O checks out in a worktree. True when Esc was taken.
    fn local_keys(&self, ui: &Ui, acts: &mut Vec<Act>) -> bool {
        if self.local.open && keymap::consume(ui, "popover.close") {
            acts.push(Act::Local(if self.local.confirming() { LocalAct::CancelConfirm } else { LocalAct::Toggle }));
            return true;
        }
        // The confirm step takes no other choice until it is answered.
        if self.local.confirming() {
            return false;
        }
        if keymap::consume(ui, "review.open_locally") {
            acts.push(Act::Local(if self.local.open { LocalAct::Editor(Editor::VsCode) } else { LocalAct::Toggle }));
        }
        if keymap::consume(ui, "review.worktree") {
            acts.push(Act::Local(LocalAct::Worktree));
        }
        false
    }

    fn keys(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        // The AI review sheet took its own keys this frame; nothing under it moves.
        if self.sheet.is_some() || self.share.is_some() {
            return;
        }
        // An open popover takes Esc before the screen's own Esc leaves the review.
        if self.options_open && keymap::consume(ui, "popover.close") {
            acts.push(Act::ToggleOptions);
            return;
        }
        if self.commits.open && keymap::consume(ui, "popover.close") {
            acts.push(Act::Commit(CommitAct::Toggle));
            return;
        }
        if self.local_keys(ui, acts) {
            return;
        }
        self.submit_key(ui, acts);
        if keymap::consume(ui, "review.share") {
            acts.push(Act::OpenShare);
        }
        // Only a focused text field takes letters; a focused hunk header or button must not.
        let typing = ui.ctx().text_edit_focused();
        self.ai_keys(ui, typing, acts);
        if keymap::pressed(ui, "review.back") {
            // In a text field, Esc only leaves the field (egui drops its focus); it never leaves the review.
            // In a read-only view Esc leaves the view and keeps the hidden composer and selection as they were.
            // An open hand-off sheet closes first, whatever has focus.
            if self.handoff.is_some() {
                acts.push(Act::CancelHandOff);
            } else {
                match (&self.composer, &self.editing, self.selection) {
                    (Some(_), _, _) if !self.readonly() => acts.push(Act::CancelComposer),
                    (_, Some(_), _) => acts.push(Act::CancelEdit),
                    _ if typing || self.was_typing => {}
                    _ if self.whole_shown() => acts.push(Act::ToggleWhole),
                    _ if self.commits.shown() => acts.push(Act::Commit(CommitAct::All)),
                    _ if self.since_shown() => acts.push(Act::ToggleSince),
                    (_, _, Some(_)) => acts.push(Act::Select { file: usize::MAX, hunk: 0, line: 0, extend: false }),
                    _ => acts.push(Act::Back),
                }
            }
        }
        if typing {
            return;
        }
        let pressed = |id: &str| keymap::pressed(ui, id);
        if pressed("review.whole_stack") {
            acts.push(Act::ToggleWhole);
        }
        if self.from_back && pressed("review.since_review") {
            acts.push(Act::ToggleSince);
        }
        if pressed("review.hand_off") {
            acts.push(Act::OpenHandOff);
        }
        let blocks = !self.readonly() && self.rendered_current();
        for (id, act) in [
            ("review.next_hunk", if blocks { Act::StepBlock { forward: true } } else { Act::Step { hunks: true, forward: true } }),
            ("review.prev_hunk", if blocks { Act::StepBlock { forward: false } } else { Act::Step { hunks: true, forward: false } }),
            ("review.next_file", Act::Step { hunks: false, forward: true }),
            ("review.prev_file", Act::Step { hunks: false, forward: false }),
        ] {
            if pressed(id) {
                acts.push(act);
            }
        }
        // Commits step in a read-only view too: they are how the reader moves between picks.
        if keymap::pressed(ui, "review.commits") {
            acts.push(Act::Commit(CommitAct::Toggle));
        }
        for (id, forward) in [("review.next_commit", true), ("review.prev_commit", false)] {
            if keymap::pressed(ui, id) {
                acts.push(Act::Commit(CommitAct::Step { forward }));
            }
        }
        // A read-only view has no drafts to step through, nothing to comment on or mark viewed.
        if self.readonly() {
            return;
        }
        self.view_keys(ui, acts);
        self.folder_keys(ui, acts);
        if self.selection.is_some() && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
            acts.push(Act::CopyLines);
        }
        for (id, act) in [
            ("review.next_draft", Act::StepDraft { forward: true }),
            ("review.prev_draft", Act::StepDraft { forward: false }),
            ("review.comment", Act::OpenComposer),
            ("review.edit_draft", Act::EditHere),
        ] {
            if pressed(id) {
                acts.push(act);
            }
        }
        // The current file may be hidden when the filter matches nothing.
        if pressed("review.viewed") && self.visible.contains(&self.file) {
            acts.push(Act::ToggleViewed(self.file));
        }
        self.diff_keys(ui, acts);
    }

    /// ⌘↵ is taken before any text field sees it. In a thread's reply field it sends that reply (through the same ask as the
    /// button), never the review; else it sends the open hand-off or saves the open comment or edit, else submits.
    fn submit_key(&self, ui: &Ui, acts: &mut Vec<Act>) {
        let replying = self.cards.typing.clone().filter(|_| ui.ctx().text_edit_focused());
        if let Some(id) = replying.filter(|_| keymap::consume(ui, "review.submit")) {
            acts.push(Act::Thread(ThreadAct::Send(id)));
        } else if keymap::consume(ui, "review.submit") {
            acts.push(match (&self.composer, &self.editing) {
                _ if self.handoff.is_some() => Act::SendHandOff,
                // The composer is not drawn in a read-only view, so ⌘↵ must not save it unseen.
                (Some(_), _) if !self.readonly() => Act::SaveComposer,
                (_, Some(_)) => Act::SaveEdit,
                _ => Act::Submit,
            });
        }
    }

    /// The keys for the diff options, more lines, and threads.
    fn diff_keys(&self, ui: &Ui, acts: &mut Vec<Act>) {
        for (id, act) in self.layout_keys() {
            if keymap::pressed(ui, id) {
                acts.push(act);
            }
        }
        for (id, forward) in [("review.next_thread", true), ("review.prev_thread", false)] {
            if keymap::pressed(ui, id) {
                acts.push(Act::ThreadStep { forward });
            }
        }
        // Resolving and replying act once per press: a held key must not flip the thread back and forth.
        for (id, act) in [("review.resolve_thread", Act::ThreadResolveCurrent), ("review.reply_thread", Act::ThreadReplyCurrent)] {
            if keymap::fresh(ui, id) {
                acts.push(act);
            }
        }
    }

    fn files_pane(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let p = theme::pal(ui);
        if self.readonly() {
            let title = if self.commits.shown() {
                "Changes in the picked commits"
            } else if self.whole_shown() {
                "Changes in the stack"
            } else {
                "Changes since your review"
            };
            components::label(ui, title, 13.5, p.graphite);
            if let Some((files, omitted, _)) = self.compared() {
                components::label(ui, components::files_word(files.len()), 12.0, p.text_tertiary);
                ui.add_space(theme::SPACE_2);
                let most = files.iter().map(|f| f.additions + f.deletions).max().unwrap_or(0);
                ScrollArea::vertical().id_salt("readonly-files").show(ui, |ui| {
                    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
                    for file in files {
                        let left_out = if omitted.contains(&file.path) { "  ·  too large" } else { "" };
                        // Two files with one name show their folder too, so the rows tell apart.
                        let twins = files.iter().filter(|other| base(&other.path) == base(&file.path)).count() > 1;
                        let name = if twins { file.path.as_str().to_string() } else { base(&file.path) };
                        components::files_changed_row(ui, &format!("{name}{left_out}"), file.additions, file.deletions, most)
                            .on_hover_text(&file.path);
                    }
                });
            }
            return;
        }
        let counted: Vec<&FileDiff> = self.view.files.iter().filter(|f| !f.generated).collect();
        let viewed = counted.iter().filter(|f| self.viewed(f)).count();
        components::padded(ui, Margin::symmetric(8, 0), |ui| {
            ui.horizontal(|ui| {
                components::label(ui, "Changes", 13.5, p.graphite);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    components::label(ui, format!("{viewed} of {} viewed", counted.len()), 12.0, p.text_tertiary);
                });
            });
        });
        ui.add_space(theme::SPACE_2);
        let field = ui.add(TextEdit::singleline(&mut self.filter_text).hint_text("Filter: internal/** !*_test.go").desired_width(f32::INFINITY));
        if field.changed() {
            acts.push(Act::FilterChanged);
        }
        ui.add_space(4.0);
        let save = format!("Save for {}", self.view.change.repo);
        if Pill::new(PillKind::Secondary, &save).small().max_width(ui.available_width()).show(ui).clicked() {
            acts.push(Act::SaveFilter);
        }
        if !self.patterns.is_empty() {
            let hidden = self.view.files.len() - self.visible.len();
            components::note(
                ui,
                format!("{} of {} · {hidden} hidden", self.visible.len(), components::files_word(self.view.files.len())),
                p.text_secondary,
            );
        }
        ui.add_space(theme::SPACE_2);
        ScrollArea::vertical().id_salt("files").show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let mut folder = None;
            for &i in &self.visible {
                let file = &self.view.files[i];
                let (dir, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
                if folder != Some(dir) {
                    folder = Some(dir);
                    if !dir.is_empty() {
                        ui.add_space(4.0);
                        let (done, total) = self.folder_counts(dir);
                        let folded = self.folded.contains(dir);
                        let head = components::folder_row(ui, dir, folded, done, total);
                        if head.clicked() {
                            acts.push(if folded { Act::UnfoldFolder(dir.to_string()) } else { Act::FoldFolder(dir.to_string()) });
                        }
                        let by_key = head.has_focus() && keymap::pressed(ui, "app.context_menu");
                        if head.secondary_clicked() || by_key {
                            let at = if by_key {
                                menu::below(head.rect)
                            } else {
                                ui.input(|input| input.pointer.interact_pos()).unwrap_or_else(|| head.rect.center())
                            };
                            acts.push(Act::FolderMenu { dir: dir.to_string(), at, restore: Some(head.id) });
                        }
                    }
                }
                if !dir.is_empty() && self.folded.contains(dir) {
                    continue;
                }
                if components::review_file_row(ui, name, i == self.file, self.viewed(file), file.additions, file.deletions).clicked() {
                    acts.push(Act::GoFile(i));
                }
            }
        });
    }

    fn diff_pane(&mut self, ui: &mut Ui, width: f32, acts: &mut Vec<Act>) {
        let p = theme::pal(ui);
        let readonly = self.readonly();
        let controls = !readonly && !self.view.files.is_empty();
        let avail = ui.available_rect_before_wrap();
        components::padded(ui, Margin { left: 20, right: 20, top: 12, bottom: 12 }, |ui| {
            ui.set_width(ui.available_width());
            ui.scope(|ui| {
                ui.set_max_width((ui.available_width() - if controls { LAYOUT_CONTROLS } else { 0.0 }).max(160.0));
                if self.commits.shown() {
                    let mut asked = Vec::new();
                    self.commits.title(ui, &mut asked);
                    acts.extend(asked.into_iter().map(Act::Commit));
                } else if self.whole_shown() {
                    self.whole_title(ui);
                } else if self.since_shown() {
                    self.since_title(ui);
                } else {
                    let meta = &self.view.meta;
                    components::label(ui, meta.title.as_str(), 15.0, p.graphite);
                    let (adds, dels) = self.view.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
                    let line = format!(
                        "{} · {} → {} · +{adds} −{dels} in {} · {}",
                        meta.author,
                        meta.head_branch,
                        meta.base_branch,
                        components::files_word(self.view.files.len()),
                        self.view.change.reference()
                    );
                    components::note_sized(ui, line, 12.0, p.text_tertiary);
                }
            });
            if controls {
                self.layout_controls(ui, avail, acts);
            }
        });
        components::rule(ui);
        components::padded(ui, Margin::symmetric(20, 6), |ui| {
            let trigger = ui.horizontal(|ui| {
                let at = components::title_action(ui, &self.commits.summary(), &keymap::label(ui, "review.commits"));
                if at.clicked() {
                    acts.push(Act::Commit(CommitAct::Toggle));
                }
                at.rect
            });
            self.commits.trigger = Some(trigger.inner);
            self.stack_strip(ui, acts);
            let hint = self.hint_line(ui);
            if !hint.is_empty() {
                components::note(ui, hint, p.text_tertiary);
            }
        });
        if readonly {
            self.readonly_body(ui, width, acts);
            self.focus = Focus::None;
            return;
        }
        if self.view.stale {
            let note = "These drafts were written on an earlier commit. They post against that commit, so the forge may show them as outdated. Edit or delete them in Your review.";
            components::padded(ui, Margin::symmetric(20, 6), |ui| components::banner(ui, BannerKind::Warn, "Drafts from an earlier commit", note));
        }
        self.threads_note(ui);
        if self.view.files.is_empty() {
            components::padded(ui, Margin::symmetric(20, 6), |ui| {
                components::note_sized(ui, "This change has no file changes.", 13.0, p.text_secondary);
            });
            return;
        }
        if self.visible.is_empty() {
            components::padded(ui, Margin::symmetric(20, 6), |ui| {
                components::label(ui, "The filter hides every file", 15.0, p.graphite);
                let note = format!(
                    "{} matches none of the {} changed files. Drafts and viewed marks are not affected.",
                    self.patterns.join(" "),
                    self.view.files.len()
                );
                components::note_sized(ui, note, 13.0, p.text_secondary);
            });
            return;
        }
        let (counted, all_viewed) = {
            let counted: Vec<&FileDiff> = self.view.files.iter().filter(|f| !f.generated).collect();
            (counted.len(), !counted.is_empty() && counted.iter().all(|f| self.viewed(f)))
        };
        if !all_viewed {
            self.show_viewed = false;
        }
        if all_viewed && !self.show_viewed {
            // Completion is an empty state too (frame 108). It claims nothing the data cannot back: generated files are not counted,
            // and drafts from an earlier commit are said to be so.
            let drafts = self.view.local.drafts.len();
            let mut body = match drafts {
                0 => "You have no drafts, so Submit posts only your verdict.".to_string(),
                1 => "Your 1 draft is ready to go out as one review.".to_string(),
                n => format!("Your {n} drafts are ready to go out as one review."),
            };
            body.push_str(" A file that changes again loses its viewed mark and comes back here.");
            let generated = self.view.files.iter().filter(|f| f.generated).count();
            if generated > 0 {
                body.push_str(&format!(
                    " {} not counted.",
                    if generated == 1 { "1 generated file is".to_string() } else { format!("{generated} generated files are") }
                ));
            }
            if self.view.stale {
                body.push_str(" The drafts were written on an earlier commit.");
            }
            let title = format!("All {counted} {} viewed", if counted == 1 { "file" } else { "files" });
            let pressed = components::padded(ui, Margin::symmetric(20, 48), |ui| {
                components::empty_state(ui, &title, &body, &[("Show viewed files", ""), ("Go to Submit", "")])
            });
            match pressed {
                Some(0) => self.show_viewed = true,
                Some(_) => self.focus_submit = true,
                None => {}
            }
            self.focus = Focus::None;
            return;
        }
        ScrollArea::both().id_salt("diff").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for n in 0..self.visible.len() {
                let i = self.visible[n];
                self.file_ui(ui, i, width, acts);
                ui.add_space(theme::SPACE_3);
            }
        });
        self.focus = Focus::None;
    }

    fn file_ui(&mut self, ui: &mut Ui, i: usize, width: f32, acts: &mut Vec<Act>) {
        let file = &self.view.files[i];
        let viewed = self.viewed(file);
        let p = theme::pal(ui);
        // Frame 79 (and every file header of the review frames): the path and its counts, then the views, a divider, Viewed and its key.
        let header = egui::Frame::new()
            .fill(p.sunken)
            .inner_margin(Margin::symmetric(16, 0))
            .show(ui, |ui| {
                // The controls take their room first; the path is cut to what is left, so they never draw over it.
                ui.allocate_ui_with_layout(egui::vec2(width - 32.0, 34.0), Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.spacing_mut().icon_spacing = 6.0;
                    components::key_cap(ui, &keymap::label(ui, "review.viewed"));
                    let mut checked = viewed;
                    let check =
                        ui.add(egui::Checkbox::new(&mut checked, RichText::new("Viewed").size(12.0 * theme::sz(ui).body / 13.5).color(p.graphite)));
                    if check.changed() {
                        acts.push(Act::ToggleViewed(i));
                    }
                    if self.focus == Focus::File && i == self.file {
                        check.request_focus();
                        check.scroll_to_me(Some(Align::TOP));
                    }
                    if views::offered(file).len() > 1 {
                        let (divider, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), egui::Sense::hover());
                        ui.painter().rect_filled(divider, 0.0, p.border);
                        self.view_switcher(ui, i, file, acts);
                    }
                    ui.add_space(12.0);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let counts = (format!("+{}", file.additions), format!("−{}", file.deletions));
                        let room =
                            ui.available_width() - 20.0 - components::mono_width(ui, &counts.0, 11.5) - components::mono_width(ui, &counts.1, 11.5);
                        let renamed = file.old_path.as_deref().map(|old| format!("{old} → ")).unwrap_or_default();
                        components::mono_label_cut(ui, format!("{renamed}{}", file.path), 12.5, p.graphite, room);
                        components::mono_label(ui, counts.0, 11.5, p.moss_text);
                        components::mono_label(ui, counts.1, 11.5, p.rose_text);
                    });
                });
            })
            .response
            .rect;
        components::seam_bottom(ui, header);
        let view = self.views.current(file);
        if view != FileView::Source && self.rendered_visible(file, viewed) {
            // A file loads when its header is near the window; the one the reader is on always does.
            let near = i == self.file || ui.is_rect_visible(header.expand2(egui::vec2(0.0, theme::PREFETCH)));
            self.rendered_body(ui, i, view, width, near, acts);
            return;
        }
        if !self.expanded(file) {
            let rendered = views::offered(file).len() > 1;
            let why = if viewed && file.binary && rendered { "Collapsed because viewed.".to_string() } else { fold_reason(file, viewed) };
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                components::label(ui, why, 12.5, p.text_tertiary);
                if (!file.binary || (viewed && rendered)) && Pill::new(PillKind::Secondary, "Show").small().show(ui).clicked() {
                    acts.push(Act::Unfold(file.path.clone()));
                }
            });
            return;
        }
        let current_hunk = (i == self.file).then_some(self.hunk);
        let (layout, split) = (self.layout.clone(), self.split_on());
        let gaps = context::gaps(&file.hunks);
        let reveals = self.reveals.get(&i).map(Vec::as_slice).unwrap_or_default();
        let keys = ExpandKeys::of(ui);
        let ai_current = self.ai_current().map(|c| c.draft.id.clone());
        let file_threads: Vec<&ReviewThread> = match &self.threads {
            Fetch::Ready(list) => list.threads.iter().filter(|t| t.path == file.path).collect(),
            _ => Vec::new(),
        };
        let mut under = UnderRow {
            width,
            file,
            forge: self.view.change.forge,
            selection: self.selection,
            composer: &mut self.composer,
            focus_composer: &mut self.focus_composer,
            editing: &mut self.editing,
            inline: &mut self.inline,
            ai_inline: &mut self.ai_inline,
            // Line numbers of stale drafts belong to another commit, so they are not placed in this diff.
            drafts: if self.view.stale { &[] } else { &self.view.local.drafts },
            ai: &self.ai_pending,
            threads: &file_threads,
            cards: &mut self.cards,
            ai_current,
            focus_draft: if self.focus_draft { self.draft_focus } else { None },
            focus_ai: self.focus_ai,
        };
        let draw = GapDraw {
            width,
            file: i,
            split,
            wrap: layout.wrap,
            keys: &keys,
            here: current_hunk,
            // An added or deleted file has no lines below its last hunk to read.
            below_last: matches!(file.status, FileStatus::Modified | FileStatus::Renamed) && !file.hunks.is_empty(),
            hunks: file.hunks.len(),
        };
        for (h, hunk) in file.hunks.iter().enumerate() {
            let writing = under.composer.is_some();
            let pinned = under.selection.filter(|s| writing && (s.file, s.hunk) == (i, h)).map(|s| s.lines().1);
            let visible = diff_view::visible(hunk, layout.hide_whitespace, pinned);
            if h > 0 {
                gap_ui(ui, &draw, gaps.get(h).copied(), reveals.get(h), acts);
            }
            let header = components::hunk_header(ui, width, &hunk_title(hunk, visible.hidden));
            if self.focus == Focus::Hunk && (i, h) == (self.file, self.hunk) {
                header.request_focus();
                header.scroll_to_me(Some(Align::TOP));
            }
            if h == 0 {
                gap_ui(ui, &draw, gaps.first().copied(), reveals.first(), acts);
            }
            let rows = HunkRows { file: i, hunk: h, source: hunk, layout: layout.clone(), split };
            hunk_rows(ui, &rows, &visible, &mut under, acts);
        }
        gap_ui(ui, &draw, gaps.get(file.hunks.len()).copied(), reveals.get(file.hunks.len()), acts);
        loose_threads(ui, &mut under, acts);
    }

    /// The line of keys under the title in a read-only view; the Keys panel lists the rest.
    fn hint_line(&self, ui: &Ui) -> String {
        let k = |id: &str| keymap::label(ui, id);
        let (hunks, files) = (
            format!("{} {} hunk", k("review.next_hunk"), k("review.prev_hunk")),
            format!("{} {} file", k("review.next_file"), k("review.prev_file")),
        );
        if self.commits.shown() {
            format!("{hunks} · {files} · {} returns to all changes", k("review.back"))
        } else if self.whole_shown() {
            format!("{hunks} · {files} · {} or {} back to this layer", k("review.whole_stack"), k("review.back"))
        } else if self.since_shown() {
            format!("{hunks} · {files} · {} or {} back to all changes", k("review.since_review"), k("review.back"))
        } else {
            String::new()
        }
    }

    /// ⇧S switches between rendered and source; 1 to 4 pick an image compare mode.
    fn view_keys(&self, ui: &Ui, acts: &mut Vec<Act>) {
        let Some(file) = self.view.files.get(self.file).filter(|_| self.visible.contains(&self.file)) else { return };
        let offered = views::offered(file);
        if offered.len() < 2 {
            return;
        }
        if keymap::pressed(ui, "review.toggle_source") {
            acts.push(Act::ToggleView(self.file));
        }
        let modes = [
            ("review.image_two_up", FileView::ImageTwoUp),
            ("review.image_swipe", FileView::ImageSwipe),
            ("review.image_onion", FileView::ImageOnion),
            ("review.image_difference", FileView::ImageDifference),
        ];
        for (id, view) in modes {
            if offered.contains(&view) && keymap::pressed(ui, id) {
                acts.push(Act::SetView(self.file, view));
            }
        }
    }

    /// The views `file` offers, in its header (frames 25, 26, 79); the Keys panel names the key that steps through them.
    fn view_switcher(&self, ui: &mut Ui, i: usize, file: &FileDiff, acts: &mut Vec<Act>) {
        let offered = views::offered(file);
        let current = self.views.current(file);
        let labels: Vec<String> = offered.iter().map(|view| view.label().to_string()).collect();
        let at = offered.iter().position(|&view| view == current).unwrap_or(0);
        if let Some(picked) = components::segmented(ui, &format!("views-{i}"), "File view", &labels, at)
            && let Some(&view) = offered.get(picked)
        {
            acts.push(Act::SetView(i, view));
        }
    }

    /// A rendered view draws unless the reader folded the file (viewed, generated or large) and has not unfolded it.
    fn rendered_visible(&self, file: &FileDiff, viewed: bool) -> bool {
        let folded = viewed || file.generated || file.additions + file.deletions > LARGE_FILE_LINES;
        !folded || self.unfolded.contains(&file.path)
    }

    /// Whether the diff draws `file` in a rendered view now, not as hunks.
    fn drawn_rendered(&self, file: &FileDiff) -> bool {
        self.views.current(file) != FileView::Source && self.rendered_visible(file, self.viewed(file))
    }

    /// The current file shows as rendered Markdown, so J K and C work on its blocks.
    fn rendered_current(&self) -> bool {
        self.view.files.get(self.file).is_some_and(|f| self.visible.contains(&self.file) && self.drawn_rendered(f) && self.views.walks_blocks(f))
    }

    /// Draws file `i` in its rendered `view`; Markdown blocks carry their composer and drafts under them.
    fn rendered_body(&mut self, ui: &mut Ui, i: usize, view: FileView, width: f32, near: bool, acts: &mut Vec<Act>) {
        let file = &self.view.files[i];
        if near {
            self.views.want(ui.ctx(), &self.view.change, &self.view.meta, file, i == self.file);
        }
        let (stale, drafts) = (self.view.stale, &self.view.local.drafts);
        let mut after = |ui: &mut Ui, item: usize, block: &MdBlockView| {
            if let Some(target) = self.block
                && (target.file, target.item) == (i, item)
                && self.composer.is_some()
            {
                let focus = std::mem::take(&mut self.focus_composer);
                let title =
                    format!("Comment on this {} · it posts on line {} of the source", views::kind_name(block.block.kind), target.line.shown());
                composer_ui(ui, Place::Line(width), &mut self.composer, focus, title, acts);
            }
            let Some(anchor) = block.anchor else { return };
            // Line numbers of stale drafts belong to another commit, so they are not placed in this file.
            for draft in drafts.iter().filter(|d| !stale && d.path == file.path && d.end == anchor) {
                self.inline.insert(draft.id);
                let focus = self.focus_draft && self.draft_focus == Some(draft.id);
                draft_card(ui, Place::Line(width), draft, &mut self.editing, focus, acts);
            }
        };
        match views::show(ui, &mut self.views, file, view, width, &mut after) {
            Some(Pressed::Comment(item)) => acts.push(Act::CommentBlock { file: i, item }),
            Some(Pressed::Select(item)) => {
                // The file the reader clicked in is the one J K C work on.
                make_current(&mut self.file, &mut self.hunk, i);
                self.views.set_cursor(&file.path, item);
            }
            None => {}
        }
        if self.views.is_svg(&file.path) {
            self.svg_band(ui, i, width, acts);
        }
    }

    /// Frame 79: under an SVG's markup, the comment being written on it and its drafts, each card on its own.
    fn svg_band(&mut self, ui: &mut Ui, i: usize, width: f32, acts: &mut Vec<Act>) {
        let file = &self.view.files[i];
        if let Some(target) = self.block.filter(|t| t.file == i)
            && self.composer.is_some()
        {
            let focus = std::mem::take(&mut self.focus_composer);
            let what = self.views.block_anchor(&file.path, target.item).map(|(_, name)| name).unwrap_or_else(|| "the rendered view".into());
            let title = format!("Comment on {what} · it posts on line {} of the source", target.line.shown());
            composer_ui(ui, Place::Band(width), &mut self.composer, focus, title, acts);
        }
        // Line numbers of stale drafts belong to another commit, so they are not placed in this file.
        for draft in self.view.local.drafts.iter().filter(|d| !self.view.stale && d.path == file.path) {
            self.inline.insert(draft.id);
            let focus = self.focus_draft && self.draft_focus == Some(draft.id);
            draft_card(ui, Place::Band(width), draft, &mut self.editing, focus, acts);
        }
    }

    /// `C` in rendered Markdown or an SVG: comment on the block or markup row the cursor is on, else the first changed one.
    fn comment_on_cursor_block(&mut self, ctx: &Context) {
        let target = self.view.files.get(self.file).and_then(|f| self.views.comment_target(&f.path));
        match target {
            Some((item, _)) => self.open_block_composer(ctx, self.file, item),
            None => {
                self.notice = Some(format!(
                    "This file has no changed block or markup row to comment on. Click one, then press {}.",
                    keymap::label(ctx, "review.comment")
                ))
            }
        }
    }

    fn open_block_composer(&mut self, ctx: &Context, file: usize, item: usize) {
        let Some(path) = self.view.files.get(file).map(|f| f.path.clone()) else { return };
        match self.views.block_anchor(&path, item) {
            Some((Some(line), _)) => {
                let target = BlockTarget { file, item, line };
                if self.block != Some(target) {
                    self.composer = Some(String::new());
                }
                self.selection = None;
                // The box belongs to the current file; otherwise the next frame closes it.
                make_current(&mut self.file, &mut self.hunk, file);
                self.block = Some(target);
                self.focus_composer = true;
                self.views.set_cursor(&path, item);
            }
            _ => {
                self.notice = Some(format!(
                    "The forge takes comments only on lines the diff shows, and this has none. Press {} to see the source.",
                    keymap::label(ctx, "review.toggle_source")
                ));
            }
        }
    }

    /// `C` in a source view: comment on the selected lines, else on the first line of the current hunk.
    fn open_source_composer(&mut self) {
        // A box left open on a rendered block must not take the source comment.
        drop_block(&mut self.block, &mut self.composer);
        if self.selection.is_none()
            && let Some(file) = self.view.files.get(self.file)
            && self.expanded(file)
            && file.hunks.get(self.hunk).is_some_and(|h| !h.lines.is_empty())
        {
            self.selection = Some(Selection { file: self.file, hunk: self.hunk, anchor: 0, head: 0 });
        }
        if self.selection.is_some() {
            self.composer.get_or_insert_with(String::new);
            self.focus_composer = true;
        }
    }

    /// `review.set_view`: this file now, and every file with its extension from now on.
    fn set_view(&mut self, i: usize, view: FileView) -> Result<(), pr_craft_engine::CommandError> {
        let Some(path) = self.view.files.get(i).map(|f| f.path.clone()) else { return Ok(()) };
        // A comment box opened on a rendered block would be hidden by another view, so it closes with it.
        if self.block.take().is_some() {
            self.composer = None;
        }
        self.views.choose(&path, view);
        self.notice = Some(format!("{path} shown as {}.", view.label()));
        self.views.remember(engine_view::set_view(&file_view::extension(&path), view)?);
        Ok(())
    }

    fn review_pane(&mut self, ui: &mut Ui, acts: &mut Vec<Act>, runs: &mut Runs) {
        let keys = self.key_rows(ui);
        let rect = ui.available_rect_before_wrap();
        // In a short window the Keys panel scrolls with the rest instead of squeezing it.
        let pinned = rect.height() >= theme::KEYS_PINNED_HEIGHT;
        let split = if pinned { rect.bottom() - components::keys_panel_height(keys.len()) } else { rect.bottom() + 4.0 };
        let top = Rect::from_min_max(rect.min, egui::pos2(rect.right(), split - 4.0));
        let bottom = Rect::from_min_max(egui::pos2(rect.left(), split), rect.max);
        ui.scope_builder(UiBuilder::new().id_salt("review-top").max_rect(top), |ui| {
            ScrollArea::vertical().id_salt("review-pane").auto_shrink([false, false]).show(ui, |ui| {
                self.drafts_pane(ui, acts);
                self.ai_panel(ui, acts);
                ui.add_space(theme::SPACE_3);
                match activity::runs_panel(ui, runs, &self.view.change) {
                    PanelAct::None => {}
                    PanelAct::RunAgain => acts.push(Act::OpenSheet),
                    PanelAct::Details(id) => acts.push(Act::Details(id)),
                }
                if !pinned {
                    ui.add_space(theme::SPACE_3);
                    components::keys_panel(ui, &keys);
                }
            });
        });
        if pinned {
            ui.scope_builder(UiBuilder::new().id_salt("review-keys").max_rect(bottom), |ui| components::keys_panel(ui, &keys));
        }
    }

    /// The rows of the Keys panel: what the keys do on this screen, with the caps the reader set.
    fn key_rows(&self, ui: &Ui) -> Vec<(Vec<String>, String)> {
        let caps = |ids: &[&str]| ids.iter().map(|id| keymap::label(ui, id)).collect::<Vec<_>>();
        let mut rows = vec![
            (caps(&["review.next_hunk", "review.prev_hunk"]), "Next or previous hunk".to_string()),
            (caps(&["review.next_file", "review.prev_file"]), "Next or previous file".to_string()),
        ];
        // A read-only view has nothing to comment on, keep or mark viewed; its own hint says how to leave it.
        if self.readonly() {
            return rows;
        }
        rows.push((caps(&["review.comment"]), "Comment on line".to_string()));
        rows.push((caps(&["review.viewed"]), "Mark file viewed".to_string()));
        rows.push((caps(&["review.layout", "review.diff_options"]), "Unified or split, diff options".to_string()));
        if matches!(&self.threads, Fetch::Ready(list) if !list.threads.is_empty()) {
            rows.push((caps(&["review.next_thread", "review.prev_thread"]), "Next or previous thread".to_string()));
            rows.push((caps(&["review.resolve_thread", "review.reply_thread"]), "Resolve or reply to the thread".to_string()));
        }
        rows.push((caps(&["review.expand_above", "review.expand_below"]), "Show more lines around the hunk".to_string()));
        if !self.ai_pending.is_empty() {
            rows.push((caps(&["review.keep_ai"]), "Keep suggestion".to_string()));
            rows.push((caps(&["review.keep_ai_edit"]), "Keep suggestion and edit".to_string()));
            rows.push((caps(&["review.discard_ai"]), "Discard suggestion".to_string()));
            rows.push((caps(&["review.next_ai", "review.prev_ai"]), "Next or previous suggestion".to_string()));
        }
        if !self.layers().is_empty() {
            rows.push((caps(&["review.whole_stack"]), "Whole stack".to_string()));
        }
        if self.from_back {
            rows.push((caps(&["review.since_review"]), "Since your review".to_string()));
        }
        if let Some(file) = self.view.files.get(self.file).filter(|_| self.visible.contains(&self.file))
            && views::offered(file).len() > 1
        {
            rows.push((caps(&["review.toggle_source"]), "Rendered or source".to_string()));
        }
        rows
    }

    fn drafts_pane(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        self.handoff_sheet(ui, acts);
        let p = theme::pal(ui);
        let local = &self.view.local;
        ui.horizontal(|ui| {
            components::label(ui, "Your review", 13.5, p.graphite);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                components::label(ui, format!("{} drafts", local.drafts.len()), 12.0, p.text_tertiary);
            });
        });
        if local.drafts.is_empty() {
            // The drafts live in this Mac's store, so an empty list is the whole truth about them.
            components::empty_state(
                ui,
                "No drafts on this Mac",
                "Comments you write are kept here, offline too, until you submit the review. Nothing posts on its own.",
                &[],
            );
        }
        ui.add_space(theme::SPACE_2);
        ui.spacing_mut().item_spacing.y = 0.0;
        for draft in &local.drafts {
            // Every draft is one row; clicking it opens its editor, under the row or on its line.
            let focus = self.focus_draft && self.draft_focus == Some(draft.id);
            let editing = matches!(&self.editing, Some((id, _)) if *id == draft.id);
            let row = components::draft_row(ui, draft.body.lines().next().unwrap_or_default(), &draft_place(draft));
            // Stepping to a draft focuses its row, but never takes focus back from the editor it opened.
            if focus && !editing && !self.inline.contains(&draft.id) {
                row.request_focus();
                row.scroll_to_me(Some(Align::Center));
            }
            if row.clicked() {
                acts.push(Act::StartEdit(draft.id));
            }
            if !self.inline.contains(&draft.id) && editing {
                draft_card(ui, Place::Panel, draft, &mut self.editing, focus, acts);
            }
        }
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.add_space(4.0);
        components::rule(ui);
        ui.add_space(8.0);
        components::label(ui, "Summary", 12.0, p.text_secondary);
        ui.add_space(4.0);
        let summary = ui.add(
            TextEdit::multiline(&mut self.body)
                .desired_width(ui.available_width())
                .desired_rows(theme::SUMMARY_ROWS)
                .hint_text("General comment, posted with the verdict"),
        );
        if summary.lost_focus() && self.body != local.body {
            acts.push(Act::SaveBody);
        }
        ui.add_space(theme::SPACE_3);
        components::label(ui, "Verdict", 12.0, p.text_secondary);
        ui.add_space(6.0);
        for &verdict in Verdict::offered(self.view.change.forge) {
            if components::verdict_card(ui, local.verdict == verdict, verdict.word(), verdict_note(verdict)).clicked() {
                acts.push(Act::Verdict(verdict));
            }
            ui.add_space(4.0);
        }
        ui.add_space(theme::SPACE_2);
        let busy = self.submitting.is_some();
        let submit = Pill::new(PillKind::Primary, if busy { "Posting…" } else { "Submit review" }).block().enabled(!busy);
        let submit = if busy { submit } else { submit.key(ui, "review.submit") };
        let submitted = submit.show(ui);
        if std::mem::take(&mut self.focus_submit) {
            submitted.request_focus();
        }
        if submitted.clicked() {
            acts.push(Act::Submit);
        }
        self.post_unknown_note(ui, acts);
        ui.add_space(theme::SPACE_2);
        if self.handoff.is_none() && Pill::new(PillKind::Secondary, "Hand off this review").key(ui, "review.hand_off").small().show(ui).clicked() {
            acts.push(Act::OpenHandOff);
        }
        // Rule 21: the outcome of a post arrives away from focus, so it is a live region.
        let status = match (&self.error, &self.notice, busy) {
            (Some(error), _, _) => Some((BannerKind::Error, error.as_str())),
            (_, Some(notice), _) => Some((BannerKind::Info, notice.as_str())),
            (_, _, true) => Some((BannerKind::Info, "Posting the review…")),
            _ if self.handing.is_some() => Some((BannerKind::Info, "Handing off the review…")),
            _ => None,
        };
        if let Some((kind, text)) = status {
            ui.add_space(theme::SPACE_2);
            let status = components::banner(ui, kind, text, "");
            ui.ctx().accesskit_node_builder(status.id, |node| node.set_live(egui::accesskit::Live::Polite));
        }
        if self.error.is_some() && Pill::new(PillKind::Secondary, "Copy as Markdown").small().show(ui).clicked() {
            acts.push(Act::CopyMarkdown);
        }
    }

    /// What a post that ran out of time leaves: the words, and, once Submit was pressed again, the question.
    fn post_unknown_note(&self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let Some(why) = &self.view.local.post_unknown else { return };
        ui.add_space(theme::SPACE_2);
        let note = components::banner(ui, BannerKind::Warn, "The last post may have gone through", why);
        ui.ctx().accesskit_node_builder(note.id, |node| node.set_live(egui::accesskit::Live::Polite));
        if self.confirm_post {
            ui.add_space(theme::SPACE_2);
            ui.horizontal(|ui| {
                if Pill::new(PillKind::Primary, "I checked: post again").small().show(ui).clicked() {
                    acts.push(Act::ConfirmPost);
                }
                if Pill::new(PillKind::Secondary, "Cancel").small().show(ui).clicked() {
                    acts.push(Act::DismissPost);
                }
            });
        }
    }

    fn whole_shown(&self) -> bool {
        !matches!(self.whole, Fetch::Idle)
    }

    fn since_shown(&self) -> bool {
        !matches!(self.since, Fetch::Idle)
    }

    /// A read-only comparison replaces the diff: the whole stack or what changed since the user's review.
    fn readonly(&self) -> bool {
        self.whole_shown() || self.since_shown() || self.commits.shown()
    }

    /// The files of the read-only comparison that has loaded, the paths the forge left out, and the forge's page for it.
    fn compared(&self) -> Option<(&[FileDiff], &[String], Option<&str>)> {
        match (&self.commits.range, &self.whole, &self.since) {
            (Fetch::Ready(range), _, _) => Some((&range.files, &range.omitted, range.web_url.as_deref())),
            (_, Fetch::Ready(whole), _) => Some((&whole.files, &whole.omitted, whole.web_url.as_deref())),
            (_, _, Fetch::Ready(since)) => Some((&since.files, &since.omitted, since.web_url.as_deref())),
            _ => None,
        }
    }

    /// The stack's layers, bottom first; empty until they load and when this change is not stacked.
    fn layers(&self) -> &[StackLayer] {
        match &self.layers {
            Fetch::Ready(layers) if layers.len() >= 2 => layers,
            _ => &[],
        }
    }

    fn toggle_whole(&mut self, ctx: &Context) {
        self.focus = Focus::None;
        self.readonly_at = (0, 0);
        if self.whole_shown() {
            self.whole = Fetch::Idle;
        } else if !self.layers().is_empty() {
            self.since = Fetch::Idle;
            self.commits.range = Fetch::Idle;
            self.commits.picked = None;
            // The strip already read the layers; the diff does not list open changes again.
            let (change, layers) = (self.view.change.clone(), self.layers().to_vec());
            self.whole = Fetch::start(ctx, move || stacks::whole_of(&change, layers).map_err(crate::fail));
        }
    }

    fn toggle_since(&mut self, ctx: &Context) {
        self.focus = Focus::None;
        self.readonly_at = (0, 0);
        if self.since_shown() {
            self.since = Fetch::Idle;
        } else {
            self.whole = Fetch::Idle;
            self.commits.range = Fetch::Idle;
            self.commits.picked = None;
            let change = self.view.change.clone();
            self.since = Fetch::start(ctx, move || turn::since_last(&change).map_err(crate::fail));
        }
    }

    /// The layers of this change's stack, bottom first. Each layer is reviewed on its own diff: its base is the layer below.
    fn stack_strip(&self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let layers = match &self.layers {
            Fetch::Idle | Fetch::Loading(_) => {
                ui.label(RichText::new("Checking whether this change is stacked…").size(theme::sz(ui).body).color(theme::pal(ui).text_tertiary));
                return;
            }
            // Rule 15: a failed lookup is not "not stacked".
            Fetch::Failed(error) => {
                let note = format!("Could not check whether this change is stacked: {error}");
                crate::live_label(ui, note, theme::pal(ui).text_secondary);
                return;
            }
            Fetch::Ready(_) => self.layers(),
        };
        if layers.is_empty() {
            return;
        }
        card(ui, |ui| {
            ui.label(
                RichText::new(format!("Stack · {} layers, bottom first", layers.len())).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary),
            );
            ScrollArea::vertical().id_salt("stack").max_height(theme::STACK_STRIP_HEIGHT).show(ui, |ui| {
                for layer in layers {
                    let draft = if layer.draft { "  ·  Draft" } else { "" };
                    let text = format!("{}{}  {}{draft}", layer.change.forge.number_sigil(), layer.change.number, layer.title);
                    if layer.change == self.view.change {
                        let here = RichText::new(format!("{text}  ·  This layer")).size(theme::sz(ui).body).color(theme::pal(ui).graphite);
                        ui.label(here.background_color(theme::pal(ui).ember_subtle));
                    } else {
                        let button = egui::Button::new(RichText::new(text).size(theme::sz(ui).body).color(theme::pal(ui).graphite)).frame(false);
                        if ui.add(button).on_hover_text(format!("Review {} on its own diff", layer.change.reference())).clicked() {
                            acts.push(Act::OpenLayer(layer.change.clone(), layer.title.clone()));
                        }
                    }
                }
            });
        });
    }

    fn whole_title(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        components::label(ui, format!("Whole stack: {} layers", self.layers().len()), 15.0, p.graphite);
        if let Fetch::Ready(whole) = &self.whole
            && let (Some(bottom), Some(top)) = (whole.layers.first(), whole.layers.last())
        {
            let (adds, dels) = whole.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
            let line = format!("{} → {} · +{adds} −{dels} in {}", top.head_branch, bottom.base_branch, components::files_word(whole.files.len()));
            components::note_sized(ui, line, 12.0, p.text_tertiary);
        }
        let note = "Read-only. Comments go on a layer: pick one in the stack above, then comment on its own diff.";
        components::note_sized(ui, note, 12.0, p.text_tertiary);
        if let Fetch::Ready(whole) = &self.whole
            && whole.truncated
        {
            let note = format!("{} left out some files because they are too large; they are marked below.", self.view.change.forge.name());
            components::note_sized(ui, note, 12.0, p.ember_text);
        }
    }

    fn since_title(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        components::label(ui, "Since your review", 15.0, p.graphite);
        if let Fetch::Ready(since) = &self.since {
            components::note_sized(ui, since_line(since, Utc::now()), 12.0, p.text_tertiary);
            if since.rewritten {
                // Rule 15: this is not "only what is new".
                let text = "The commit you reviewed was force-pushed away, so what is new cannot be told apart. This shows everything in the change since its branch meets the base.";
                let banner = components::banner(ui, BannerKind::Warn, "Your reviewed commit is gone", text);
                ui.ctx().accesskit_node_builder(banner.id, |node| node.set_live(egui::accesskit::Live::Polite));
            }
        }
        let note = format!(
            "Read-only. To comment, press {} or {} for all changes.",
            keymap::label(ui, "review.since_review"),
            keymap::label(ui, "review.back")
        );
        components::note_sized(ui, note, 12.0, p.text_tertiary);
        if let Fetch::Ready(since) = &self.since
            && since.truncated
        {
            let note = format!("{} left out some files because they are too large; they are marked below.", self.view.change.forge.name());
            components::note_sized(ui, note, 12.0, p.ember_text);
        }
    }

    /// The body of a read-only comparison: its files as the review draws them, with nothing to select or comment on.
    fn readonly_body(&self, ui: &mut Ui, width: f32, acts: &mut Vec<Act>) {
        ui.add_space(theme::SPACE_2);
        let what = if self.commits.shown() {
            "the picked commits"
        } else if self.whole_shown() {
            "the whole stack"
        } else {
            "what changed since your review"
        };
        let Some((files, omitted, web_url)) = self.compared() else {
            match (&self.commits.range, &self.whole, &self.since) {
                (Fetch::Failed(error), _, _) | (_, Fetch::Failed(error), _) | (_, _, Fetch::Failed(error)) => {
                    crate::live_label(ui, format!("Could not load {what}: {error}"), theme::pal(ui).rose_text);
                }
                _ => crate::live_label(ui, format!("Fetching {what}…"), theme::pal(ui).text_secondary),
            }
            return;
        };
        if files.is_empty() {
            let empty = match &self.since {
                _ if self.commits.shown() => "The picked commits change no file.",
                Fetch::Ready(since) if since.commits_since == 0 => "No new commits since your review.",
                Fetch::Ready(_) => "No file changed since your review.",
                _ => "The stack has no file changes.",
            };
            ui.label(RichText::new(empty).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
            return;
        }
        let state = WholeView {
            omitted,
            unfolded: &self.unfolded,
            at: self.readonly_at,
            focus: self.focus,
            forge: self.view.change.forge.name(),
            web_url,
            width,
        };
        ScrollArea::both().id_salt("readonly").auto_shrink([false, false]).show(ui, |ui| {
            for (i, file) in files.iter().enumerate() {
                whole_file_ui(ui, i, file, &state, acts);
                ui.add_space(theme::SPACE_3);
            }
        });
    }

    fn poll_handoff(&mut self) {
        let Some(rx) = &self.handing else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the hand-off stopped before the forge answered".into()),
        };
        self.handing = None;
        match result {
            Ok(done) => {
                self.error = None;
                self.notice = Some(done.message);
                self.handoff = None;
            }
            // The sheet stays open so the login can be fixed.
            Err(error) => self.error = Some(error),
        }
    }

    /// Starts `review.hand_off` on its own thread: it writes to the forge, so it never blocks a frame.
    fn send_handoff(&mut self, ctx: &Context) {
        let Some(to) = self.handoff.clone().filter(|_| self.handing.is_none()) else { return };
        self.error = None;
        self.notice = None;
        let (tx, rx) = mpsc::channel();
        let (ctx, change) = (ctx.clone(), self.view.change.clone());
        std::thread::spawn(move || {
            let _ = tx.send(turn::hand_off(&change, &to).map_err(crate::fail));
            ctx.request_repaint();
        });
        self.handing = Some(rx);
    }

    /// The hand-off sheet (`H`): who takes over, and what the forge is asked to do.
    fn handoff_sheet(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let Some(to) = &mut self.handoff else { return };
        let (focus, busy) = (std::mem::take(&mut self.focus_handoff), self.handing.is_some());
        let (reference, forge) = (self.view.change.reference(), self.view.change.forge.name());
        card(ui, |ui| {
            ui.label(RichText::new(format!("Hand off {reference}")).size(theme::sz(ui).body).color(theme::pal(ui).graphite));
            let note =
                "Someone else finishes this review. They are asked to review and you are removed as a reviewer. Your comments stay on the change.";
            ui.label(RichText::new(note).size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
            ui.label(RichText::new("Who takes over").size(theme::sz(ui).body).color(theme::pal(ui).text_secondary));
            let field = ui.add(TextEdit::singleline(to).hint_text(format!("{forge} login")).desired_width(f32::INFINITY));
            if focus {
                field.request_focus();
            }
            ui.horizontal(|ui| {
                if ui.button(keymap::hint(ui, "Cancel", "review.back")).clicked() {
                    acts.push(Act::CancelHandOff);
                }
                let label = if busy { "Handing off…".to_string() } else { keymap::hint(ui, "Hand off", "review.submit") };
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                    acts.push(Act::SendHandOff);
                }
            });
        });
    }

    fn poll_submit(&mut self) {
        let Some(rx) = &self.submitting else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("posting stopped before the forge answered".into()),
        };
        self.submitting = None;
        match result {
            Ok(local) => {
                self.notice = Some(format!("Review posted to {}.", self.view.change.forge.name()));
                self.set_local(local);
            }
            Err(error) => {
                self.error = Some(error);
                // A GitLab post may have sent some drafts before it failed; show what is left.
                if let Ok(local) = engine::local(&self.view.change) {
                    self.set_local(local);
                }
            }
        }
    }

    fn set_local(&mut self, local: LocalReview) {
        // Text typed into the summary since the last save stays; only an unedited field follows the file.
        if self.body == self.view.local.body {
            self.body.clone_from(&local.body);
        }
        self.view.local = local;
    }

    /// Steps through files (N, P) or hunks (J, K) of the visible, unfolded files, or of the read-only view when it shows.
    fn step(&mut self, hunks: bool, forward: bool) {
        let readonly = self.readonly();
        let next = if let Some((files, ..)) = self.compared() {
            let shown: Vec<usize> = (0..files.len()).collect();
            let counts = |i: usize| files.get(i).filter(|f| shows_hunks(f, false, &self.unfolded)).map_or(0, |f| f.hunks.len());
            next_stop(&shown, &counts, self.readonly_at, hunks, forward)
        } else if readonly {
            None
        } else {
            let counts = |i: usize| self.view.files.get(i).filter(|f| self.expanded(f) && !self.drawn_rendered(f)).map_or(0, |f| f.hunks.len());
            next_stop(&self.visible, &counts, (self.file, self.hunk), hunks, forward)
        };
        if let Some(stop) = next {
            if readonly {
                self.readonly_at = stop;
            } else {
                (self.file, self.hunk) = stop;
            }
            self.focus = if hunks { Focus::Hunk } else { Focus::File };
        }
    }

    /// Moves to the next or previous draft in file order and focuses its card (`[` and `]`).
    fn step_draft(&mut self, forward: bool) {
        let rank = |path: &str| self.view.files.iter().position(|f| f.path == path).unwrap_or(usize::MAX);
        let mut order: Vec<(usize, u32, u64)> = self.view.local.drafts.iter().map(|d| (rank(&d.path), d.end.shown(), d.id)).collect();
        order.sort_unstable();
        let at = self.draft_focus.and_then(|id| order.iter().position(|o| o.2 == id));
        let next = match (at, forward) {
            (None, true) => order.first(),
            (None, false) => order.last(),
            (Some(p), true) => order.get(p + 1).or(order.get(p)),
            (Some(p), false) => order.get(p.saturating_sub(1)),
        };
        if let Some(&(file, _, id)) = next {
            self.draft_focus = Some(id);
            self.focus_draft = true;
            if let Some(&i) = self.visible.iter().find(|&&i| i == file) {
                self.file = i;
            }
        }
    }

    fn apply(&mut self, ctx: &Context, acts: Vec<Act>, runs: &mut Runs) -> Outcome {
        // Acts change what the next frame draws; without this it waits for the next input event.
        if !acts.is_empty() {
            ctx.request_repaint();
        }
        for act in acts {
            let result = match act {
                Act::Back | Act::OpenLayer(..) if self.submitting.is_some() || self.handing.is_some() || self.writing.is_some() => {
                    self.notice = Some(WAIT_FOR_FORGE.into());
                    Ok(())
                }
                Act::Back => return Outcome::Back,
                Act::ToggleWhole => {
                    self.toggle_whole(ctx);
                    Ok(())
                }
                Act::ToggleSince => {
                    self.toggle_since(ctx);
                    Ok(())
                }
                Act::OpenShare => {
                    if self.share.is_none() {
                        // The sheet's field takes typing: a text field of the review must not keep focus under it.
                        if let Some(id) = ctx.memory(|m| m.focused()) {
                            ctx.memory_mut(|m| m.surrender_focus(id));
                        }
                        self.share = Some(ShareSheet::open());
                    }
                    Ok(())
                }
                Act::OpenSheet => {
                    if self.sheet.is_none() {
                        // The sheet takes letter keys: a text field of the review must not keep focus under it.
                        if let Some(id) = ctx.memory(|m| m.focused()) {
                            ctx.memory_mut(|m| m.surrender_focus(id));
                        }
                        self.sheet = Some(AiSheet::open(ctx, self.view.change.clone(), self.view.local.viewed.len()));
                    }
                    Ok(())
                }
                Act::StopRuns => {
                    let mine: Vec<_> = runs.of(&self.view.change).cloned().collect();
                    activity::stop_change(ctx, runs, &mine);
                    Ok(())
                }
                Act::Details(id) => {
                    self.run_detail = Some((id, None));
                    Ok(())
                }
                Act::KeepAi { id, edit } => {
                    self.keep_ai(runs, id, edit);
                    Ok(())
                }
                Act::DiscardAi(id) => {
                    self.discard_ai(runs, id);
                    Ok(())
                }
                Act::FocusAi(id) => {
                    self.ai_focus = Some(id);
                    Ok(())
                }
                Act::StepAi { forward } => {
                    self.step_ai(forward);
                    Ok(())
                }
                Act::OpenHandOff => {
                    self.handoff.get_or_insert_with(String::new);
                    self.focus_handoff = true;
                    Ok(())
                }
                Act::CancelHandOff => {
                    self.handoff = None;
                    Ok(())
                }
                Act::SendHandOff => {
                    self.send_handoff(ctx);
                    Ok(())
                }
                Act::OpenLayer(change, title) => return Outcome::Open(change, title),
                Act::StepDraft { forward } => {
                    self.step_draft(forward);
                    Ok(())
                }
                Act::Select { file, hunk, line, extend } => {
                    self.block = None;
                    self.selection = match self.selection {
                        _ if file == usize::MAX => None,
                        Some(s) if extend && (s.file, s.hunk) == (file, hunk) => Some(Selection { head: line, ..s }),
                        _ => Some(Selection { file, hunk, anchor: line, head: line }),
                    };
                    if file != usize::MAX {
                        (self.file, self.hunk) = (file, hunk);
                    }
                    Ok(())
                }
                Act::GoFile(file) => {
                    drop_block(&mut self.block, &mut self.composer);
                    (self.file, self.hunk, self.focus) = (file, 0, Focus::File);
                    Ok(())
                }
                Act::Step { hunks, forward } => {
                    self.step(hunks, forward);
                    Ok(())
                }
                Act::ToggleViewed(i) => match self.view.files.get(i) {
                    Some(file) => {
                        let hash = (!self.viewed(file)).then_some(file.hash);
                        engine::mark_viewed(&self.view.change, &file.path, hash).map(|local| self.set_local(local))
                    }
                    None => Ok(()),
                },
                Act::Unfold(path) => {
                    self.unfolded.insert(path);
                    Ok(())
                }
                Act::OpenComposer => {
                    if self.rendered_current() {
                        self.comment_on_cursor_block(ctx);
                    } else if let Some(file) = self.view.files.get(self.file).filter(|f| self.drawn_rendered(f)) {
                        // Rule 15: no invisible composer; say where comments go.
                        self.notice = Some(format!(
                            "{} is shown rendered and a comment here has no source line to post on. Press {} for the source diff, then {}.",
                            file.path,
                            keymap::label(ctx, "review.toggle_source"),
                            keymap::label(ctx, "review.comment")
                        ));
                    } else {
                        self.open_source_composer();
                    }
                    Ok(())
                }
                Act::SetView(i, view) => self.set_view(i, view),
                Act::ToggleView(i) => match self.view.files.get(i).map(|f| file_view::toggled(&f.path, self.views.current(f))) {
                    Some(view) => self.set_view(i, view),
                    None => Ok(()),
                },
                Act::StepBlock { forward } => {
                    let path = self.view.files.get(self.file).map(|f| f.path.clone());
                    // At the last changed block, or with none, J and K go on to the next file like N and P.
                    if !path.is_some_and(|path| self.views.step_block(&path, forward)) {
                        self.step(false, forward);
                    }
                    Ok(())
                }
                Act::CommentBlock { file, item } => {
                    self.open_block_composer(ctx, file, item);
                    Ok(())
                }
                Act::SetLayout(options) => self.set_layout(options),
                Act::ToggleOptions => {
                    self.options_open = !self.options_open;
                    Ok(())
                }
                Act::Expand { file, gap, kind } => {
                    self.queue_expand(ctx, file, gap, kind);
                    Ok(())
                }
                Act::Commit(act) => {
                    self.commit_act(ctx, act);
                    Ok(())
                }
                Act::Local(act) => {
                    let target = self.open_target();
                    self.local.act(ctx, &self.view.change, act, target.as_ref());
                    Ok(())
                }
                Act::Thread(act) => {
                    self.thread_act(ctx, act);
                    Ok(())
                }
                Act::ThreadStep { forward } => {
                    self.step_thread(forward);
                    Ok(())
                }
                Act::ThreadResolveCurrent => {
                    self.resolve_current(ctx);
                    Ok(())
                }
                Act::ThreadReplyCurrent => {
                    self.reply_current();
                    Ok(())
                }
                Act::ExpandAround(kind) => {
                    self.expand_around(ctx, kind);
                    Ok(())
                }
                Act::SaveComposer => self.save_composer(),
                Act::CancelComposer => {
                    self.composer = None;
                    self.block = None;
                    Ok(())
                }
                Act::StartEdit(id) => {
                    self.editing = self.view.local.drafts.iter().find(|d| d.id == id).map(|d| (id, d.body.clone()));
                    // The editor takes focus on the next frame, wherever the draft is drawn.
                    self.draft_focus = Some(id);
                    self.focus_draft = true;
                    Ok(())
                }
                Act::EditHere => {
                    let here = self.view.files.get(self.file).map(|f| f.path.as_str());
                    let drafts = &self.view.local.drafts;
                    let id = self
                        .draft_focus
                        .filter(|id| drafts.iter().any(|d| d.id == *id && Some(d.path.as_str()) == here))
                        .or_else(|| drafts.iter().find(|d| Some(d.path.as_str()) == here).map(|d| d.id));
                    match id.and_then(|id| drafts.iter().find(|d| d.id == id)) {
                        Some(draft) => {
                            self.editing = Some((draft.id, draft.body.clone()));
                            self.draft_focus = Some(draft.id);
                            self.focus_draft = true;
                        }
                        None => {
                            self.notice =
                                Some(format!("No draft here to edit. Press {} to go to the next draft.", keymap::label(ctx, "review.next_draft")))
                        }
                    }
                    Ok(())
                }
                Act::SaveEdit => match self.editing.take() {
                    Some((id, body)) => engine::draft_edit(&self.view.change, id, body).map(|local| self.set_local(local)),
                    None => Ok(()),
                },
                Act::CancelEdit => {
                    self.editing = None;
                    Ok(())
                }
                Act::DeleteDraft(id) => engine::draft_delete(&self.view.change, id).map(|local| self.set_local(local)),
                Act::FilterChanged => {
                    self.refilter();
                    Ok(())
                }
                Act::SaveFilter => {
                    let change = &self.view.change;
                    engine::save_filter(change.forge, change.host.as_deref(), &change.repo, self.patterns.clone())
                        .map(|()| self.notice = Some(format!("Filter saved for {}.", self.view.change.repo)))
                }
                Act::Verdict(verdict) => engine::compose(&self.view.change, verdict, self.body.clone()).map(|local| self.set_local(local)),
                Act::SaveBody => engine::compose(&self.view.change, self.view.local.verdict, self.body.clone()).map(|local| self.set_local(local)),
                Act::Submit => match self.editing.take() {
                    // ⌘↵ or Submit while a draft is open for editing posts the edited text.
                    Some((id, body)) => engine::draft_edit(&self.view.change, id, body).and_then(|local| {
                        self.set_local(local);
                        self.submit(ctx)
                    }),
                    None => self.submit(ctx),
                },
                Act::ConfirmPost => {
                    self.confirm_post = false;
                    self.submit_with(ctx, true)
                }
                Act::DismissPost => {
                    self.confirm_post = false;
                    Ok(())
                }
                Act::CopyMarkdown => engine::markdown(&self.view.change, &self.view.meta.title).map(|text| ctx.copy_text(text)),
                Act::LinesMenu { file, hunk, line, at, restore } => {
                    self.open_lines_menu(ctx, file, hunk, line, at, restore);
                    Ok(())
                }
                Act::FolderMenu { dir, at, restore } => {
                    self.open_folder_menu(ctx, dir, at, restore);
                    Ok(())
                }
                Act::FoldFolder(dir) => {
                    self.folded.insert(dir);
                    Ok(())
                }
                Act::UnfoldFolder(dir) => {
                    self.folded.remove(&dir);
                    Ok(())
                }
                Act::FoldAll => {
                    let dirs: Vec<String> =
                        self.visible.iter().filter_map(|&i| self.view.files.get(i)).map(|f| parent_dir(&f.path).to_string()).collect();
                    self.folded.extend(dirs.into_iter().filter(|d| !d.is_empty()));
                    Ok(())
                }
                Act::ViewedFolder(dir) => self.mark_folder(&dir),
                Act::CopyText { text, said } => {
                    ctx.copy_text(text);
                    self.notice = Some(said);
                    Ok(())
                }
                Act::CopyLines => {
                    if let Some((text, count)) = self.selected_lines() {
                        ctx.copy_text(text);
                        self.notice = Some(format!("Copied {count} {}.", if count == 1 { "line" } else { "lines" }));
                    }
                    Ok(())
                }
                Act::FilterTo(text) => {
                    self.filter_text = text;
                    self.refilter();
                    Ok(())
                }
                Act::OpenUrl(url) => {
                    ctx.open_url(OpenUrl::new_tab(url));
                    Ok(())
                }
                Act::OpenOnForge => {
                    ctx.open_url(OpenUrl::new_tab(&self.view.meta.url));
                    Ok(())
                }
            };
            if let Err(error) = result {
                self.error = Some(crate::fail(error));
            }
        }
        Outcome::Stay
    }

    fn save_block_comment(&mut self, target: BlockTarget) -> Result<(), pr_craft_engine::CommandError> {
        let (Some(file), Some(body)) = (self.view.files.get(target.file), self.composer.clone()) else { return Ok(()) };
        let new = NewDraft {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            start: None,
            end: target.line,
            body,
            anchor: self.view.anchor.clone(),
            on_rendered: self.views.is_svg(&file.path),
        };
        let local = engine::draft_add(&self.view.change, new)?;
        self.set_local(local);
        self.composer = None;
        self.block = None;
        self.error = None;
        Ok(())
    }

    fn save_composer(&mut self) -> Result<(), pr_craft_engine::CommandError> {
        if let Some(target) = self.block {
            return self.save_block_comment(target);
        }
        let (Some(sel), Some(body)) = (self.selection, self.composer.clone()) else { return Ok(()) };
        let Some(file) = self.view.files.get(sel.file) else { return Ok(()) };
        let Some(hunk) = file.hunks.get(sel.hunk) else { return Ok(()) };
        let (lo, hi) = sel.lines();
        let at = |l: usize| hunk.lines.get(l).map(|line| LineRef { old: line.old, new: line.new });
        let Some(end) = at(hi) else { return Ok(()) };
        let new = NewDraft {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            start: (lo != hi).then(|| at(lo)).flatten(),
            end,
            body,
            anchor: self.view.anchor.clone(),
            on_rendered: false,
        };
        let local = engine::draft_add(&self.view.change, new)?;
        self.set_local(local);
        self.composer = None;
        self.selection = None;
        self.error = None;
        Ok(())
    }

    fn submit(&mut self, ctx: &Context) -> Result<(), pr_craft_engine::CommandError> {
        self.submit_with(ctx, false)
    }

    /// Posts the review. After a post that ran out of time, it first asks (`confirmed` is the reader's yes), because that post may
    /// already have gone through and posting again could post twice.
    fn submit_with(&mut self, ctx: &Context, confirmed: bool) -> Result<(), pr_craft_engine::CommandError> {
        if self.submitting.is_some() {
            return Ok(());
        }
        let local = engine::compose(&self.view.change, self.view.local.verdict, self.body.clone())?;
        self.set_local(local);
        if !confirmed && self.view.local.post_unknown.is_some() {
            self.confirm_post = true;
            return Ok(());
        }
        self.error = None;
        self.notice = None;
        let (tx, rx) = mpsc::channel();
        let (ctx, change) = (ctx.clone(), self.view.change.clone());
        std::thread::spawn(move || {
            let _ = tx.send(engine::submit(&change, confirmed).map_err(crate::fail));
            ctx.request_repaint();
        });
        self.submitting = Some(rx);
        Ok(())
    }

    /// Split view is on and the window has room for it.
    fn split_on(&self) -> bool {
        self.layout.split && diff_view::split_allowed(self.window)
    }

    /// The keys that change the diff options or read more lines, with what each does now.
    fn layout_keys(&self) -> [(&'static str, Act); 8] {
        let now = self.layout.clone();
        [
            ("review.layout", Act::SetLayout(LayoutOptions { split: !self.split_on(), ..now.clone() })),
            ("review.hide_whitespace", Act::SetLayout(LayoutOptions { hide_whitespace: !now.hide_whitespace, ..now.clone() })),
            ("review.word_highlight", Act::SetLayout(LayoutOptions { word_highlight: !now.word_highlight, ..now.clone() })),
            ("review.wrap", Act::SetLayout(LayoutOptions { wrap: !now.wrap, ..now.clone() })),
            ("review.diff_options", Act::ToggleOptions),
            ("review.expand_above", Act::ExpandAround(Kind::Above)),
            ("review.expand_below", Act::ExpandAround(Kind::Below)),
            ("review.expand_all", Act::ExpandAround(Kind::All)),
        ]
    }

    /// `review.set_layout`: saves the options for this repository and draws with them. Split is refused when the window is
    /// too narrow, and the reader is told.
    fn set_layout(&mut self, mut wanted: LayoutOptions) -> Result<(), pr_craft_engine::CommandError> {
        if wanted.split && !diff_view::split_allowed(self.window) {
            wanted.split = false;
            self.notice = Some(format!(
                "Split view needs a window at least {} px wide; this one is {} px. The diff stays unified.",
                diff_view::MIN_SPLIT_WINDOW as u32,
                self.window as u32
            ));
        }
        self.layout = engine::set_layout(self.view.change.forge, self.view.change.host.as_deref(), &self.view.change.repo, wanted)?;
        Ok(())
    }

    /// The Unified/Split switch and the Diff options control, at the right of the diff header (frame 91).
    fn layout_controls(&mut self, ui: &mut Ui, header: Rect, acts: &mut Vec<Act>) {
        let rect = Rect::from_min_size(egui::pos2(header.right() - 20.0 - LAYOUT_CONTROLS, header.top() + 12.0), egui::vec2(LAYOUT_CONTROLS, 26.0));
        let now = self.layout.clone();
        let at = usize::from(self.split_on());
        let mut trigger = None;
        ui.scope_builder(UiBuilder::new().max_rect(rect).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let both = ["Unified".to_string(), "Split".to_string()];
            if let Some(picked) = components::segmented(ui, "layout", "Diff layout", &both, at) {
                acts.push(Act::SetLayout(LayoutOptions { split: picked == 1, ..now.clone() }));
            }
            let options = components::title_action(ui, "Diff options", &keymap::label(ui, "review.diff_options"));
            trigger = Some(options.rect);
            if options.clicked() {
                acts.push(Act::ToggleOptions);
            }
        });
        self.options_trigger = trigger;
    }

    /// Reads more lines for a gap, after the read that is on its way.
    fn queue_expand(&mut self, ctx: &Context, file: usize, gap: usize, kind: Kind) {
        self.expand_queue.push_back((file, gap, kind));
        self.next_expand(ctx);
    }

    /// ⇧K, ⇧J and ⌥E: more lines next to the current hunk.
    fn expand_around(&mut self, ctx: &Context, kind: Kind) {
        let Some(file) = self.view.files.get(self.file).filter(|f| self.visible.contains(&self.file) && self.expanded(f)) else { return };
        if file.hunks.is_empty() || self.drawn_rendered(file) {
            return;
        }
        let (above, below) = (self.hunk, self.hunk + 1);
        let gaps: &[usize] = match kind {
            Kind::Above => &[above],
            Kind::Below => &[below],
            Kind::All => &[above, below],
        };
        if !gaps.iter().any(|&gap| self.has_more(self.file, gap, kind)) {
            self.notice = Some("There are no more lines to show around this hunk.".into());
            return;
        }
        for &gap in gaps {
            self.queue_expand(ctx, self.file, gap, kind);
        }
    }

    /// Whether reading more of gap `gap` of file `file` that way would bring lines.
    fn has_more(&self, file: usize, gap: usize, kind: Kind) -> bool {
        let Some(f) = self.view.files.get(file) else { return false };
        // Below the last hunk there are lines to read only in a file that exists on both sides; an added or deleted file has no such lines.
        if gap >= f.hunks.len() && (f.hunks.is_empty() || !matches!(f.status, FileStatus::Modified | FileStatus::Renamed)) {
            return false;
        }
        let Some(&at) = context::gaps(&f.hunks).get(gap) else { return false };
        let reveal = self.reveals.get(&file).and_then(|r| r.get(gap)).cloned().unwrap_or_default();
        context::plan(at, &reveal, kind).is_some()
    }

    /// Starts the next queued read when none is on its way. A gap with nothing to read that way is skipped.
    fn next_expand(&mut self, ctx: &Context) {
        while self.expanding.is_none() {
            let Some((file, gap, kind)) = self.expand_queue.pop_front() else { return };
            let Some(f) = self.view.files.get(file) else { continue };
            let Some(&at) = context::gaps(&f.hunks).get(gap) else { continue };
            let reveal = self.reveals.get(&file).and_then(|r| r.get(gap)).cloned().unwrap_or_default();
            let Some((from, to)) = context::plan(at, &reveal, kind) else { continue };
            let (change, meta, path) = (self.view.change.clone(), self.view.meta.clone(), f.path.clone());
            let fetch = Fetch::start(ctx, move || engine::expand_at(&change, &meta, &path, engine_view::Side::Head, from, to).map_err(crate::fail));
            self.expanding = Some(Expanding { file, gap, kind, fetch });
        }
    }

    /// Puts the lines a read brought into the gap they were read for, then starts the next read.
    fn poll_expand(&mut self, ctx: &Context) {
        let Some(reading) = &mut self.expanding else { return };
        reading.fetch.poll();
        let done = match &reading.fetch {
            Fetch::Ready(read) => Ok(read.clone()),
            Fetch::Failed(error) => Err(error.clone()),
            Fetch::Idle | Fetch::Loading(_) => return,
        };
        let Some(reading) = self.expanding.take() else { return };
        match done {
            Ok(read) => self.reveal(reading.file, reading.gap, reading.kind, read),
            Err(error) => {
                self.expand_queue.clear();
                self.error = Some(format!("Could not read more lines: {error}"));
            }
        }
        self.next_expand(ctx);
    }

    fn reveal(&mut self, file: usize, gap: usize, kind: Kind, read: engine::Expanded) {
        let Some(f) = self.view.files.get(file) else { return };
        let all = context::gaps(&f.hunks);
        let Some(&at) = all.get(gap) else { return };
        let reveals = self.reveals.entry(file).or_default();
        if reveals.len() < all.len() {
            reveals.resize(all.len(), Reveal::default());
        }
        if let Some(reveal) = reveals.get_mut(gap) {
            context::apply(at, reveal, kind, read.lines, Some(read.total));
        }
    }

    /// Starts reading the change's commits.
    fn start_commits(&mut self, ctx: &Context) {
        let change = self.view.change.clone();
        self.commits.list = Fetch::start(ctx, move || commits_engine::list(&change).map_err(crate::fail));
    }

    fn commit_act(&mut self, ctx: &Context, act: CommitAct) {
        match act {
            CommitAct::Toggle => {
                self.commits.open = !self.commits.open;
                if self.commits.open && matches!(self.commits.list, Fetch::Idle | Fetch::Failed(_)) {
                    self.start_commits(ctx);
                }
            }
            CommitAct::All => {
                self.commits.picked = None;
                self.commits.anchor = None;
                self.load_range(ctx);
            }
            CommitAct::Since => {
                let range = self.commits.ready().and_then(|list| list.reviewed.as_ref().and_then(|r| pick::since(&list.commits, &r.commit)));
                if range.is_some() {
                    self.commits.picked = range;
                    self.commits.anchor = range.map(|r| r.first);
                    self.say_merges();
                    self.load_range(ctx);
                }
            }
            CommitAct::DropMerges => {
                let cut = self.commits.ready().zip(self.commits.picked).and_then(|(list, p)| pick::without_merges(&list.commits, p));
                if cut.is_some() {
                    self.commits.picked = cut;
                    self.commits.anchor = cut.map(|r| r.first);
                    self.load_range(ctx);
                }
            }
            CommitAct::Pick { index, shift } => {
                self.commits.picked = pick::click(self.commits.picked, index, shift, self.commits.anchor);
                if !shift {
                    self.commits.anchor = Some(index);
                }
                self.say_merges();
                self.load_range(ctx);
            }
            CommitAct::Step { forward } => {
                let Some(count) = self.commits.ready().map(|list| list.commits.len()) else {
                    self.notice = Some("Reading the commits. Press the key again when the list is in.".into());
                    if matches!(self.commits.list, Fetch::Idle | Fetch::Failed(_)) {
                        self.start_commits(ctx);
                    }
                    return;
                };
                self.commits.picked = pick::step(self.commits.picked, count, forward);
                self.commits.anchor = self.commits.picked.map(|p| p.first);
                self.say_merges();
                self.load_range(ctx);
            }
        }
    }

    /// Says so when the pick holds a merge commit, since its diff then also holds the merged-in work.
    fn say_merges(&mut self) {
        let merges = self.commits.ready().zip(self.commits.picked).map(|(list, p)| pick::merges(&list.commits, p).len()).unwrap_or(0);
        if merges > 0 {
            let s = if merges == 1 { "" } else { "s" };
            self.notice = Some(format!(
                "This pick holds {merges} merge commit{s}, so the diff also holds what was merged in. “Leave out merge commits” in the title cuts it down."
            ));
        }
    }

    /// Shows the diff between the first picked commit's parent and the last picked commit, or the whole change again when
    /// nothing is picked.
    fn load_range(&mut self, ctx: &Context) {
        self.focus = Focus::None;
        self.readonly_at = (0, 0);
        self.whole = Fetch::Idle;
        self.since = Fetch::Idle;
        let Some(picked) = self.commits.picked else {
            self.commits.range = Fetch::Idle;
            return;
        };
        let Some(list) = self.commits.ready() else { return };
        let bounds = pick::bounds(&list.commits, picked).map(|(from, to)| (from.to_string(), to.to_string()));
        match bounds {
            Ok((from, to)) => {
                let change = self.view.change.clone();
                self.commits.range = Fetch::start(ctx, move || commits_engine::range(&change, &from, &to).map_err(crate::fail));
            }
            Err(error) => {
                self.commits.picked = None;
                self.commits.range = Fetch::Idle;
                self.error = Some(error);
            }
        }
    }

    /// The Commits to review popover (frame 95), under the Commits control.
    fn commits_popover(&mut self, ctx: &Context, acts: &mut Vec<Act>) {
        if !self.commits.open {
            return;
        }
        let at = self.commits.trigger.map_or(egui::pos2(300.0, 120.0), |t| t.left_bottom() + egui::vec2(0.0, 4.0));
        let totals = self.view.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
        let mut asked = Vec::new();
        let shown = self.commits.popover(ctx, at, totals, &mut asked);
        acts.extend(asked.into_iter().map(Act::Commit));
        // A press anywhere else closes it; a press on the control that opened it is that control's to handle.
        let trigger = self.commits.trigger;
        let away = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !shown.contains(p) && !trigger.is_some_and(|t| t.contains(p)))
        });
        if away {
            acts.push(Act::Commit(CommitAct::Toggle));
        }
    }

    /// The Open locally popover (frame 96), under the Open control.
    fn local_popover(&mut self, ctx: &Context, acts: &mut Vec<Act>) {
        if !self.local.open {
            return;
        }
        let target = self.open_target();
        let mut asked = Vec::new();
        let shown = self.local.popover(ctx, &self.view.change, target.as_ref(), &mut asked);
        acts.extend(asked.into_iter().map(Act::Local));
        // A press anywhere else closes it, unless it is the confirm step; a press on the control that opened it is that control's.
        let trigger = self.local.trigger.get();
        // A press inside an open context menu (its "Open … in VS Code" item) is not away: the menu's choice runs on release.
        let menu = menu::rect(ctx);
        let away = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !shown.contains(p) && !trigger.is_some_and(|t| t.contains(p)) && !menu.is_some_and(|m| m.contains(p)))
        });
        if away && !self.local.confirming() {
            acts.push(Act::Local(LocalAct::Toggle));
        }
    }

    /// The file and line the editors open: the selected line, else the top of the current hunk. None in a read-only view and
    /// for a deleted file, whose lines are not in the checkout.
    fn open_target(&self) -> Option<Target> {
        let file = self.current_file().filter(|f| f.status != FileStatus::Deleted).filter(|_| !self.readonly())?;
        let selected = self.selection.filter(|s| s.file == self.file);
        let hunk = file.hunks.get(selected.map_or(self.hunk, |s| s.hunk))?;
        let from = selected.map_or(0, |s| s.head);
        // A removed line has no number in the head, so the nearest line at or after it, else before it, names the place.
        let line = hunk.lines.iter().skip(from).chain(hunk.lines.iter().take(from).rev()).find_map(|l| l.new)?;
        Some(Target { path: file.path.clone(), line })
    }

    /// Why the threads are not all on screen: still being read, failed (never "no threads"), or only part of them read.
    fn threads_note(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        let (kind, title, text) = match &self.threads {
            Fetch::Idle => return,
            Fetch::Loading(_) => {
                components::padded(ui, Margin::symmetric(20, 2), |ui| components::note(ui, "Reading the review threads…", p.text_tertiary));
                return;
            }
            Fetch::Failed(error) => {
                (BannerKind::Warn, "Could not read the review threads", format!("{error}. The diff below shows no thread cards."))
            }
            Fetch::Ready(list) => {
                let elsewhere = list.threads.iter().filter(|t| !self.view.files.iter().any(|f| f.path == t.path)).count();
                let mut said = Vec::new();
                if !list.complete {
                    said.extend(list.note.clone());
                }
                if elsewhere > 0 {
                    said.push(format!("{elsewhere} threads are on files this diff does not show."));
                }
                let undrawn = list
                    .threads
                    .iter()
                    .filter(|t| self.view.files.iter().position(|f| f.path == t.path).is_some_and(|i| !self.thread_drawn(i)))
                    .count();
                if undrawn > 0 {
                    said.push(format!("{undrawn} threads are in files not drawn as a diff right now (folded, viewed, rendered or a read-only view), so T, {} and {} skip them.", keymap::label(ui, "review.resolve_thread"), keymap::label(ui, "review.reply_thread")));
                }
                if said.is_empty() {
                    return;
                }
                (BannerKind::Info, "Review threads", said.join(" "))
            }
        };
        components::padded(ui, Margin::symmetric(20, 6), |ui| components::banner(ui, kind, title, &text));
    }

    /// The current file when the filter leaves it visible.
    fn current_file(&self) -> Option<&FileDiff> {
        self.view.files.get(self.file).filter(|_| self.visible.contains(&self.file))
    }

    /// Whether file `i` is drawn as hunks the reader can see its threads in: not folded, not shown rendered, not in a read-only view.
    fn thread_drawn(&self, i: usize) -> bool {
        !self.readonly() && self.view.files.get(i).is_some_and(|f| self.expanded(f) && !self.drawn_rendered(f))
    }

    /// The review threads of the files on screen in file and line order, as `(file, line, id)`. A file the filter hides has none.
    fn thread_order(&self) -> Vec<(usize, u32, String)> {
        let Fetch::Ready(list) = &self.threads else { return Vec::new() };
        let mut order: Vec<(usize, u32, String)> = list
            .threads
            .iter()
            .filter_map(|t| {
                let file = self.view.files.iter().position(|f| f.path == t.path).filter(|i| self.visible.contains(i) && self.thread_drawn(*i))?;
                Some((file, t.line.map_or(u32::MAX, |l| l.shown()), t.id.clone()))
            })
            .collect();
        order.sort_unstable();
        order
    }

    fn thread_by_id(&self, id: &str) -> Option<&ReviewThread> {
        match &self.threads {
            Fetch::Ready(list) => list.threads.iter().find(|t| t.id == id),
            _ => None,
        }
    }

    /// T and ⇧T: the next or previous thread becomes the one ⇧R and ⌥R act on, and its Resolve button takes focus.
    fn step_thread(&mut self, forward: bool) {
        let order = self.thread_order();
        let at = self.cards.current.as_deref().and_then(|id| order.iter().position(|o| o.2 == id));
        let next = match (at, forward) {
            (None, true) => order.first(),
            (None, false) => order.last(),
            (Some(p), true) => order.get(p + 1).or(order.get(p)),
            (Some(p), false) => order.get(p.saturating_sub(1)),
        };
        let Some((file, _, id)) = next.cloned() else {
            self.notice = Some(if self.threads.loading() {
                "The review threads are still being read.".into()
            } else {
                "No review threads in the files on screen.".into()
            });
            return;
        };
        make_current(&mut self.file, &mut self.hunk, file);
        self.cards.open.insert(id.clone());
        self.cards.current = Some(id.clone());
        self.cards.focus = Some((id, threads::Focus::Resolve));
    }

    /// The thread ⇧R and ⌥R act on: only the one stepped to with T, and only while its file is the current, visible one.
    /// There is no guess: acting on "the first thread of the file" would resolve a conversation the reader never looked at.
    fn current_thread(&self) -> Option<String> {
        let id = self.cards.current.as_deref()?;
        let order = self.thread_order();
        order.iter().find(|o| o.2 == id && o.0 == self.file && self.visible.contains(&o.0)).map(|o| o.2.clone())
    }

    /// Forgets the stepped thread once the reader is on another file or the filter hides its file.
    fn drop_stale_thread(&mut self) {
        if self.cards.current.is_some() && self.current_thread().is_none() {
            self.cards.current = None;
        }
    }

    fn resolve_current(&mut self, ctx: &Context) {
        let Some(id) = self.current_thread() else {
            self.notice = Some(format!(
                "No thread is selected. Press {} to step to one, then {} again.",
                keymap::label(ctx, "review.next_thread"),
                keymap::label(ctx, "review.resolve_thread")
            ));
            return;
        };
        match self.thread_by_id(&id).map(|t| (t.can_resolve, t.resolved, threads::place(t))) {
            Some((false, resolved, place)) => {
                let what = if resolved { "reopen" } else { "resolve" };
                self.notice = Some(format!("You cannot {what} the thread at {place} on {}.", self.view.change.forge.name()));
            }
            _ => self.write_resolve(ctx, id),
        }
    }

    fn reply_current(&mut self) {
        if let Some(id) = self.current_thread() {
            self.cards.open.insert(id.clone());
            self.cards.focus = Some((id, threads::Focus::Reply));
        }
    }

    /// The review holds a draft on the line of this thread, so a reply now and the draft's post would both land there.
    fn overlaps_draft(&self, id: &str) -> bool {
        let Some(thread) = self.thread_by_id(id) else { return false };
        let Some(line) = thread.line else { return false };
        self.view.local.drafts.iter().any(|d| d.path == thread.path && d.end.shown() == line.shown())
    }

    fn thread_act(&mut self, ctx: &Context, act: ThreadAct) {
        match act {
            ThreadAct::Toggle(id) => {
                if !self.cards.open.remove(&id) {
                    self.cards.open.insert(id);
                }
            }
            ThreadAct::AskFirst(id) => self.cards.confirm = Some(id),
            ThreadAct::CancelConfirm => self.cards.confirm = None,
            ThreadAct::Reply(id) => self.write_reply(ctx, id),
            // ⌘↵ in the field: the button's own rule, a second press confirms.
            ThreadAct::Send(id) if self.overlaps_draft(&id) && self.cards.confirm.as_deref() != Some(id.as_str()) => self.cards.confirm = Some(id),
            ThreadAct::Send(id) => self.write_reply(ctx, id),
            ThreadAct::Resolve(id) => self.write_resolve(ctx, id),
        }
    }

    /// `thread.reply`: posts the typed reply to the forge now, on its own thread of work.
    fn write_reply(&mut self, ctx: &Context, id: String) {
        let body = self.cards.replies.get(&id).cloned().unwrap_or_default();
        if body.trim().is_empty() || self.writing.is_some() {
            return;
        }
        self.cards.confirm = None;
        let (forge, place) = (self.view.change.forge.name(), self.thread_by_id(&id).map(threads::place).unwrap_or_default());
        self.start_write(ctx, id, true, move |change, id| {
            thread_engine::reply(change, id, body).map(|()| format!("Reply to the thread at {place} posted to {forge}."))
        });
    }

    /// `thread.resolve`: resolves the thread, or reopens it when it is resolved. Only where the forge lets this user.
    fn write_resolve(&mut self, ctx: &Context, id: String) {
        let Some((resolved, can, place)) = self.thread_by_id(&id).map(|t| (t.resolved, t.can_resolve, threads::place(t))) else { return };
        if self.writing.is_some() || !can {
            return;
        }
        let forge = self.view.change.forge.name();
        let to = !resolved;
        self.start_write(ctx, id, false, move |change, id| {
            thread_engine::resolve(change, id, to).map(|()| format!("{} the thread at {place} on {forge}.", if to { "Resolved" } else { "Reopened" }))
        });
    }

    fn start_write(
        &mut self,
        ctx: &Context,
        thread: String,
        reply: bool,
        job: impl FnOnce(&ChangeRef, &str) -> Result<String, pr_craft_engine::CommandError> + Send + 'static,
    ) {
        self.error = None;
        self.notice = None;
        self.cards.busy = Some(thread.clone());
        let (tx, rx) = mpsc::channel();
        let (ctx, change, id) = (ctx.clone(), self.view.change.clone(), thread.clone());
        std::thread::spawn(move || {
            let _ = tx.send(job(&change, &id).map_err(crate::fail));
            ctx.request_repaint();
        });
        self.writing = Some(ThreadWrite { rx, thread, reply });
    }

    /// Collects the forge's answer to a reply or a resolve, then reads the threads again so the card shows what the forge holds.
    fn poll_write(&mut self, ctx: &Context) {
        let Some(write) = &self.writing else { return };
        let result = match write.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("posting stopped before the forge answered".into()),
        };
        let Some(write) = self.writing.take() else { return };
        self.cards.busy = None;
        match result {
            Ok(done) => {
                if write.reply {
                    self.cards.replies.remove(&write.thread);
                }
                self.notice = Some(done);
                let change = self.view.change.clone();
                self.threads = Fetch::start(ctx, move || thread_engine::list(&change).map_err(crate::fail));
            }
            // The typed reply stays in its field.
            Err(error) => self.error = Some(error),
        }
    }

    /// The Diff options popover (frame 91), under the layout switch: the layout, whitespace, words and wrapping, saved for
    /// the repository.
    fn options_popover(&mut self, ctx: &Context, diff: Rect, acts: &mut Vec<Act>) {
        if !self.options_open || self.readonly() {
            return;
        }
        let p = theme::pal(ctx);
        let now = self.layout.clone();
        let whitespace = self.view.files.get(self.file).map_or(0, |f| f.hunks.iter().map(|h| diff_view::visible(h, true, None).hidden).sum::<u32>());
        let repo = self.view.change.repo.clone();
        let split_at = usize::from(self.split_on());
        let area = egui::Area::new(egui::Id::NULL.with("diff-options"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(egui::pos2(diff.right() - 16.0, diff.top() + 64.0))
            .show(ctx, |ui| {
                components::card_frame(ui).show(ui, |ui| {
                    ui.set_width(300.0);
                    components::label(ui, "Diff options", 13.5, p.graphite);
                    ui.add_space(theme::SPACE_2);
                    ui.horizontal(|ui| {
                        components::label(ui, "Layout", 12.5, p.text_secondary);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let both = ["Unified".to_string(), "Split".to_string()];
                            if let Some(picked) = components::segmented(ui, "options-layout", "Diff layout", &both, split_at) {
                                acts.push(Act::SetLayout(LayoutOptions { split: picked == 1, ..now.clone() }));
                            }
                        });
                    });
                    let found = format!("{whitespace} whitespace-only {} in this file", if whitespace == 1 { "line" } else { "lines" });
                    let rows = [
                        (
                            "Hide whitespace changes",
                            found.as_str(),
                            now.hide_whitespace,
                            LayoutOptions { hide_whitespace: !now.hide_whitespace, ..now.clone() },
                        ),
                        ("Highlight changed words", "", now.word_highlight, LayoutOptions { word_highlight: !now.word_highlight, ..now.clone() }),
                        ("Wrap long lines", "", now.wrap, LayoutOptions { wrap: !now.wrap, ..now.clone() }),
                    ];
                    for (title, note, on, next) in rows {
                        ui.add_space(theme::SPACE_2);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                components::label(ui, title, 12.5, p.graphite);
                                if !note.is_empty() {
                                    components::note(ui, note, p.text_tertiary);
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if components::toggle(ui, on, title).is_some() {
                                    acts.push(Act::SetLayout(next));
                                }
                            });
                        });
                    }
                    ui.add_space(theme::SPACE_2);
                    let note = format!("Saved for {repo}. Split needs a window {} px wide or more.", diff_view::MIN_SPLIT_WINDOW as u32);
                    components::note(ui, note, p.text_tertiary);
                });
            });
        // A press anywhere else closes it; a press on the control that opened it is that control's to handle.
        let away = ctx
            .input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|at| !self.options_trigger.is_some_and(|t| t.contains(at))));
        if away && !area.response.contains_pointer() {
            acts.push(Act::ToggleOptions);
        }
    }
}

/// Why a file shows no lines until the reader asks.
fn fold_reason(file: &FileDiff, viewed: bool) -> String {
    let lines = file.additions + file.deletions;
    if file.binary {
        "Binary file, not shown.".to_string()
    } else if viewed {
        "Collapsed because viewed.".to_string()
    } else if file.generated {
        format!("Generated file, folded: {lines} changed lines. It does not count toward viewed.")
    } else {
        format!("Large file, folded: {lines} changed lines.")
    }
}

/// A reply or a resolve on its way to the forge.
struct ThreadWrite {
    rx: Receiver<Result<String, String>>,
    thread: String,
    /// A reply: its text leaves the field once the forge has it.
    reply: bool,
}

/// The threads that sit on no line of the current diff (outdated, or the forge placed them nowhere), after the file's last hunk.
fn loose_threads(ui: &mut Ui, under: &mut UnderRow<'_>, acts: &mut Vec<Act>) {
    let mut asked = Vec::new();
    for thread in under.threads.iter().filter(|t| !t.placed()) {
        threads::thread_ui(ui, under.width, under.forge, thread, under.cards, false, &mut asked);
    }
    acts.extend(asked.into_iter().map(Act::Thread));
}

/// A read of lines for one gap of one file, on its way.
struct Expanding {
    file: usize,
    gap: usize,
    kind: Kind,
    fetch: Fetch<engine::Expanded>,
}

/// A hunk's `@@` line, with how many whitespace-only changes were folded away.
fn hunk_title(hunk: &Hunk, hidden: u32) -> String {
    match hidden {
        0 => hunk.header.clone(),
        1 => format!("{}  ·  1 whitespace-only line hidden", hunk.header),
        n => format!("{}  ·  {n} whitespace-only lines hidden", hunk.header),
    }
}

/// The keys of the expand bars, as the reader set them.
struct ExpandKeys {
    above: String,
    below: String,
    all: String,
}

impl ExpandKeys {
    fn of(ui: &Ui) -> Self {
        ExpandKeys {
            above: keymap::label(ui, "review.expand_above"),
            below: keymap::label(ui, "review.expand_below"),
            all: keymap::label(ui, "review.expand_all"),
        }
    }
}

/// What the gaps of one file need to draw.
struct GapDraw<'a> {
    width: f32,
    file: usize,
    split: bool,
    wrap: bool,
    keys: &'a ExpandKeys,
    /// The hunk J and K are on, when this is the current file: its bars show the keys.
    here: Option<usize>,
    /// Lines may follow the last hunk.
    below_last: bool,
    hunks: usize,
}

/// A gap: the lines read from it next to the hunk above, the bar that stands for the rest, and the lines next to the hunk below.
fn gap_ui(ui: &mut Ui, draw: &GapDraw<'_>, gap: Option<Gap>, reveal: Option<&Reveal>, acts: &mut Vec<Act>) {
    let Some(gap) = gap else { return };
    if gap.index == draw.hunks && !draw.below_last {
        return;
    }
    let none = Reveal::default();
    let reveal = reveal.unwrap_or(&none);
    revealed_rows(ui, draw, &reveal.top);
    bar_ui(ui, draw, gap, reveal, acts);
    revealed_rows(ui, draw, &reveal.bottom);
}

/// Lines read from the file: unchanged context. The forge takes comments only on lines of the diff, so these cannot be selected.
fn revealed_rows(ui: &mut Ui, draw: &GapDraw<'_>, lines: &[DiffLine]) {
    for line in lines {
        if draw.split {
            let cell = || Cell { line, selected: false, marks: &[] };
            components::split_row(ui, draw.width, [Some(cell()), Some(cell())], draw.wrap, false);
        } else {
            components::diff_row(ui, draw.width, line, false, false, CodeStyle { marks: &[], wrap: draw.wrap });
        }
    }
}

/// The bar for the lines still hidden in a gap, with the ways to show them.
fn bar_ui(ui: &mut Ui, draw: &GapDraw<'_>, gap: Gap, reveal: &Reveal, acts: &mut Vec<Act>) {
    let (hidden, range, known) = match context::remaining(gap, reveal) {
        Remaining::None => return,
        Remaining::Lines { first, last } => {
            let count = last - first + 1;
            (format!("{count} hidden {}", if count == 1 { "line" } else { "lines" }), format!("lines {first}–{last}"), Some(count))
        }
        Remaining::Unknown { first } => ("Lines below the last hunk".to_string(), format!("from line {first}"), None),
    };
    let below_hunk = gap.index < draw.hunks;
    // The hunk the keys act on: the bar above it, and the bar below it, show those keys.
    let (over, under) = (draw.here == Some(gap.index), gap.index > 0 && draw.here == Some(gap.index - 1));
    let count = |kind| context::plan(gap, reveal, kind).map(|(from, to)| to.saturating_sub(from).saturating_add(1));
    let mut buttons: Vec<(String, Option<String>, Kind)> = Vec::new();
    if known.is_none_or(|n| n > context::STEP) {
        if let Some(n) = below_hunk.then(|| count(Kind::Above)).flatten() {
            buttons.push((format!("Show {n} lines above"), over.then(|| draw.keys.above.clone()), Kind::Above));
        }
        if let Some(n) = (gap.index > 0).then(|| count(Kind::Below)).flatten() {
            buttons.push((format!("Show {n} lines below"), under.then(|| draw.keys.below.clone()), Kind::Below));
        }
    }
    let all = if known.is_some() { "Expand all" } else { "Show the rest of the file" };
    buttons.push((all.to_string(), (over || under).then(|| draw.keys.all.clone()), Kind::All));
    let shown: Vec<BarButton<'_>> = buttons.iter().map(|(label, key, _)| BarButton { label, key: key.clone() }).collect();
    if let Some((_, _, kind)) = components::expand_bar(ui, draw.width, &hidden, &range, &shown).and_then(|i| buttons.get(i)) {
        acts.push(Act::Expand { file: draw.file, gap: gap.index, kind: *kind });
    }
}

/// One hunk to draw: where it is, its lines, and the options in force.
struct HunkRows<'a> {
    file: usize,
    hunk: usize,
    source: &'a Hunk,
    layout: LayoutOptions,
    split: bool,
}

/// What a row of a hunk shows under it: the comment box, drafts, AI drafts and threads that sit on its lines.
struct UnderRow<'a> {
    width: f32,
    file: &'a FileDiff,
    forge: pr_craft_core::Forge,
    selection: Option<Selection>,
    composer: &'a mut Option<String>,
    focus_composer: &'a mut bool,
    editing: &'a mut Option<(u64, String)>,
    inline: &'a mut HashSet<u64>,
    ai_inline: &'a mut HashSet<String>,
    drafts: &'a [Draft],
    ai: &'a [ai::Pending],
    threads: &'a [&'a ReviewThread],
    cards: &'a mut threads::Cards,
    ai_current: Option<String>,
    /// The draft whose card takes focus this frame.
    focus_draft: Option<u64>,
    focus_ai: bool,
}

impl UnderRow<'_> {
    /// Draws what sits under the row that shows `lines` (each with its place in the hunk's own lines).
    fn draw(&mut self, ui: &mut Ui, at: &HunkRows<'_>, lines: &[(usize, &DiffLine)], acts: &mut Vec<Act>) {
        self.composer_under(ui, at, lines, acts);
        let (threads, drafts, ai) = (self.threads, self.drafts, self.ai);
        let mut asked = Vec::new();
        for thread in threads.iter().filter(|t| t.placed() && t.line.is_some_and(|line| lines.iter().any(|(_, shown)| line.on(shown)))) {
            let overlap = drafts.iter().any(|d| d.path == thread.path && thread.line.is_some_and(|line| d.end.shown() == line.shown()));
            threads::thread_ui(ui, self.width, self.forge, thread, self.cards, overlap, &mut asked);
        }
        acts.extend(asked.into_iter().map(Act::Thread));
        for draft in drafts.iter().filter(|d| d.path == self.file.path && lines.iter().any(|(_, line)| d.end.on(line))) {
            self.inline.insert(draft.id);
            let focus = self.focus_draft == Some(draft.id);
            draft_card(ui, Place::Line(self.width), draft, self.editing, focus, acts);
        }
        // AI drafts read at this commit sit under their line, never posted until kept and submitted.
        let on_row = |p: &&ai::Pending| {
            p.placed && p.draft.finding.path == self.file.path && lines.iter().any(|(_, line)| ai::finding_at(&p.draft.finding, line))
        };
        for pending in ai.iter().filter(on_row) {
            self.ai_inline.insert(pending.draft.id.clone());
            let current = self.ai_current.as_deref() == Some(pending.draft.id.as_str());
            ai_card(ui, Some(self.width), &pending.draft, None, current, self.focus_ai && current, acts);
        }
    }

    /// The comment box sits under the last selected line.
    fn composer_under(&mut self, ui: &mut Ui, at: &HunkRows<'_>, lines: &[(usize, &DiffLine)], acts: &mut Vec<Act>) {
        let Some(sel) = self.selection else { return };
        if self.composer.is_none() || (sel.file, sel.hunk) != (at.file, at.hunk) || !lines.iter().any(|(index, _)| *index == sel.lines().1) {
            return;
        }
        let shown = |l: usize| at.source.lines.get(l).map_or(0, |line| LineRef { old: line.old, new: line.new }.shown());
        let (lo, hi) = sel.lines();
        let mut title = if lo == hi { format!("Comment on line {}", shown(lo)) } else { format!("Comment on lines {}–{}", shown(lo), shown(hi)) };
        if lo != hi && self.forge == pr_craft_core::Forge::GitLab {
            title.push_str(&format!(". GitLab pins it to line {}; the range goes in the text.", shown(hi)));
        }
        let focus = std::mem::take(self.focus_composer);
        composer_ui(ui, Place::Line(self.width), self.composer, focus, title, acts);
    }
}

/// The folder of a path: everything before the last `/`, empty for a file at the root.
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn drag_key() -> egui::Id {
    egui::Id::NULL.with("gutter-drag")
}

/// The file and hunk a drag from the gutter button began in, while the button is still held.
fn dragging(ui: &Ui) -> Option<(usize, usize)> {
    ui.ctx().data(|d| d.get_temp::<Option<(usize, usize)>>(drag_key())).flatten()
}

/// Whether a folder name cannot be written into the Files filter: it splits on spaces and commas, `*` and `?` are globs and a
/// leading `!` excludes.
pub(crate) fn filter_unfit(dir: &str) -> bool {
    dir.contains([' ', ',', '*', '?']) || dir.starts_with('!')
}

impl Ready {
    /// How many of the visible files of folder `dir` are viewed, and how many there are.
    fn folder_counts(&self, dir: &str) -> (usize, usize) {
        let files: Vec<&FileDiff> = self.visible.iter().filter_map(|&i| self.view.files.get(i)).filter(|f| parent_dir(&f.path) == dir).collect();
        (files.iter().filter(|f| self.viewed(f)).count(), files.len())
    }

    /// ← → ⇧V ⌥← and ⌥⌘C act on the folder of the current file.
    fn folder_keys(&self, ui: &Ui, acts: &mut Vec<Act>) {
        let Some(file) = self.current_file() else { return };
        let dir = parent_dir(&file.path).to_string();
        if keymap::pressed(ui, "review.fold_all") {
            acts.push(Act::FoldAll);
        }
        if dir.is_empty() {
            return;
        }
        if keymap::pressed(ui, "review.fold_folder") {
            acts.push(Act::FoldFolder(dir.clone()));
        }
        if keymap::pressed(ui, "review.unfold_folder") {
            acts.push(Act::UnfoldFolder(dir.clone()));
        }
        if keymap::pressed(ui, "review.viewed_folder") {
            acts.push(Act::ViewedFolder(dir.clone()));
        }
        if keymap::pressed(ui, "review.copy_path") {
            acts.push(Act::CopyText { text: dir, said: "Copied the folder's path.".into() });
        }
    }

    /// What marking folder `dir` does: the mark to set (viewed, unless every visible file of it already is) and the files whose
    /// mark changes, each with its hash.
    fn folder_marks(&self, dir: &str) -> (bool, Vec<(String, u64)>) {
        let files: Vec<(&FileDiff, bool)> =
            self.visible.iter().filter_map(|&i| self.view.files.get(i)).filter(|f| parent_dir(&f.path) == dir).map(|f| (f, self.viewed(f))).collect();
        let want = !files.iter().all(|(_, viewed)| *viewed);
        (want, files.into_iter().filter(|(_, viewed)| *viewed != want).map(|(f, _)| (f.path.clone(), f.hash)).collect())
    }

    /// Marks the visible files of `dir` viewed, or all unviewed when every one already is.
    fn mark_folder(&mut self, dir: &str) -> Result<(), pr_craft_engine::CommandError> {
        let (want, changing) = self.folder_marks(dir);
        for (path, hash) in changing {
            let local = engine::mark_viewed(&self.view.change, &path, want.then_some(hash))?;
            self.set_local(local);
        }
        Ok(())
    }

    /// The text of the selected lines and how many there are.
    fn selected_lines(&self) -> Option<(String, usize)> {
        let sel = self.selection?;
        let (lo, hi) = sel.lines();
        let lines = self.view.files.get(sel.file)?.hunks.get(sel.hunk)?.lines.get(lo..=hi)?;
        Some((lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n"), lines.len()))
    }

    /// The menu of a line of the diff (frame 103), or of the selection when the line is inside it. Every item repeats a key or
    /// a button; Suggest a change is not built (it has no key, so S stays the whole-stack key), Ask Claude Code on a selection
    /// and Copy link to lines have no command, and Show the whole file is not built.
    fn open_lines_menu(&mut self, ctx: &Context, file: usize, hunk: usize, line: usize, at: egui::Pos2, restore: Option<egui::Id>) {
        if !self.selection.is_some_and(|s| s.contains(file, hunk, line)) {
            self.block = None;
            self.selection = Some(Selection { file, hunk, anchor: line, head: line });
        }
        let Some(sel) = self.selection else { return };
        // Every item acts on the file the menu names, so the current file follows the selection it was opened on.
        (self.file, self.hunk) = (sel.file, sel.hunk);
        let Some(diff) = self.view.files.get(sel.file) else { return };
        let Some(source) = diff.hunks.get(sel.hunk) else { return };
        let (lo, hi) = sel.lines();
        let shown = |l: usize| source.lines.get(l).map_or(0, |x| LineRef { old: x.old, new: x.new }.shown());
        let name = diff.path.rsplit('/').next().unwrap_or(&diff.path);
        let (header, comment) = if lo == hi {
            (format!("Line {} · {name}", shown(lo)), "Comment on this line")
        } else {
            (format!("Lines {}–{} · {name}", shown(lo), shown(hi)), "Comment on these lines")
        };
        let key = |id: &str| keymap::label(ctx, id);
        // The same two paths as ⌘O: with the popover closed it opens the popover; open, it opens the editor at this line.
        let code = match (self.open_target(), self.local.open) {
            (Some(_), false) => menu::Item::new("locally", "Open locally…").key(key("review.open_locally")),
            (Some(target), true) => menu::Item::new("vscode", format!("Open {name}:{} in VS Code", target.line)).key(key("review.open_locally")),
            (None, _) => menu::Item::new("vscode", "Open in VS Code").key(key("review.open_locally")).unavailable("This line is not in the checkout"),
        };
        let viewed = self.viewed(diff);
        use menu::Entry::{Item, Rule};
        menu::open(
            ctx,
            menu::Spec {
                owner: "lines",
                target: String::new(),
                header: Some(header),
                at,
                restore,
                entries: vec![
                    Item(menu::Item::new("comment", comment).key(key("review.comment"))),
                    Item(menu::Item::new("suggest", "Suggest a change").unavailable("Not built yet")),
                    Rule,
                    Item(menu::Item::new("copy_lines", if lo == hi { "Copy line" } else { "Copy lines" }).key("⌘C")),
                    Item(menu::Item::new("markdown", "Copy review as Markdown")),
                    Rule,
                    Item(code),
                    Rule,
                    Item(
                        menu::Item::new("whitespace", "Hide whitespace changes")
                            .key(key("review.hide_whitespace"))
                            .checked(self.layout.hide_whitespace),
                    ),
                    Item(menu::Item::new("viewed", if viewed { "Mark file unviewed" } else { "Mark file viewed" }).key(key("review.viewed"))),
                ],
            },
        );
    }

    /// The menu of a folder of the Files list (frame 105). Review this folder with AI, Open in VS Code and Reveal in Finder are
    /// not here: the AI sheet has no folder scope, and the editors open the checkout, not a folder of it.
    fn open_folder_menu(&mut self, ctx: &Context, dir: String, at: egui::Pos2, restore: Option<egui::Id>) {
        let (done, total) = self.folder_counts(&dir);
        let folded = self.folded.contains(&dir);
        // ⇧V, ← → and ⌥⌘C act on the folder of the current file, so a hint is shown only on that folder's own menu.
        let current = self.view.files.get(self.file).is_some_and(|f| parent_dir(&f.path) == dir);
        let key = |id: &str| if current { keymap::label(ctx, id) } else { String::new() };
        let unfit = filter_unfit(&dir).then_some("The folder name has characters the filter cannot hold");
        let only = |item: menu::Item| match unfit {
            Some(why) => item.unavailable(why),
            None => item,
        };
        use menu::Entry::{Item, Rule};
        menu::open(
            ctx,
            menu::Spec {
                owner: "folder",
                header: Some(format!("{dir} · {}", components::files_word(total))),
                target: dir,
                at,
                restore,
                entries: vec![
                    Item(
                        menu::Item::new("viewed", if done == total { "Mark folder unviewed" } else { "Mark folder viewed" })
                            .key(key("review.viewed_folder")),
                    ),
                    Item(if folded {
                        menu::Item::new("fold", "Open folder").key(key("review.unfold_folder"))
                    } else {
                        menu::Item::new("fold", "Collapse folder").key(key("review.fold_folder"))
                    }),
                    Item(menu::Item::new("fold_all", "Collapse all").key(keymap::label(ctx, "review.fold_all"))),
                    Rule,
                    Item(only(menu::Item::new("only", "Show only this folder"))),
                    Item(only(menu::Item::new("hide", "Hide this folder"))),
                    Rule,
                    Item(menu::Item::new("copy", "Copy path").key(key("review.copy_path"))),
                ],
            },
        );
    }

    /// Turns the choice made in an open menu into the acts of the key or button it repeats. Also ends a drag from the gutter
    /// button once the pointer button is up.
    fn menu_acts(&mut self, ctx: &Context, acts: &mut Vec<Act>) {
        if !ctx.input(|i| i.pointer.primary_down()) {
            ctx.data_mut(|d| d.insert_temp(drag_key(), None::<(usize, usize)>));
        }
        if let Some(choice) = menu::take(ctx, "lines") {
            let now = self.layout.clone();
            let act = match choice.item {
                "comment" => Some(Act::OpenComposer),
                "copy_lines" => Some(Act::CopyLines),
                "markdown" => Some(Act::CopyMarkdown),
                // Decided when the menu opened: the item that was drawn is the one that runs.
                "vscode" => Some(Act::Local(LocalAct::Editor(Editor::VsCode))),
                "locally" => Some(Act::Local(LocalAct::Toggle)),
                "whitespace" => Some(Act::SetLayout(LayoutOptions { hide_whitespace: !now.hide_whitespace, ..now.clone() })),
                "viewed" => Some(Act::ToggleViewed(self.file)),
                _ => None,
            };
            acts.extend(act);
        }
        if let Some(choice) = menu::take(ctx, "folder") {
            let dir = choice.target;
            let act = match choice.item {
                "viewed" => Some(Act::ViewedFolder(dir)),
                "fold" if self.folded.contains(&dir) => Some(Act::UnfoldFolder(dir)),
                "fold" => Some(Act::FoldFolder(dir)),
                "fold_all" => Some(Act::FoldAll),
                "only" => Some(Act::FilterTo(format!("{dir}/**"))),
                "hide" => Some(Act::FilterTo(format!("{} !{dir}/**", self.filter_text.trim()).trim().to_string())),
                "copy" => Some(Act::CopyText { text: dir, said: "Copied the folder's path.".into() }),
                _ => None,
            };
            acts.extend(act);
        }
        if let Some(choice) = menu::take(ctx, "thread") {
            self.cards.current = Some(choice.target);
            let act = match choice.item {
                "reply" => Some(Act::ThreadReplyCurrent),
                "resolve" => Some(Act::ThreadResolveCurrent),
                "next" => Some(Act::ThreadStep { forward: true }),
                "prev" => Some(Act::ThreadStep { forward: false }),
                "markdown" => Some(Act::CopyMarkdown),
                "forge" => Some(Act::OpenOnForge),
                _ => None,
            };
            acts.extend(act);
        }
    }
}

/// The rows of one hunk, unified or split, each with what sits under it.
/// The place in the hunk's visible lines that `half` of split row `row` shows: the old side of the left half, the new side of the
/// right. A removed line with no partner has no right half and an added one no left half.
fn split_pos(row: &diff_view::SplitRow, half: Half) -> Option<usize> {
    match half {
        Half::Left => row.left,
        Half::Right => row.right,
    }
}

/// What a test reads off a review: the current file, the selected lines (text, count), whether the Open locally popover is open,
/// and the viewed mark a folder would set with the files it changes.
#[cfg(test)]
pub(crate) type Probe = (usize, Option<(String, usize)>, bool, (bool, Vec<String>));

#[cfg(test)]
mod split_tests {
    use pr_craft_core::diff::parse_unified;

    use super::*;

    const DIFF: &str = "diff --git a/f.go b/f.go
--- a/f.go
+++ b/f.go
@@ -10,6 +10,6 @@ func f() {
 keep
-old one
-old two
+new one
 middle
-gone
 tail
+fresh
";

    #[test]
    fn each_half_of_a_split_row_anchors_where_the_unified_view_anchors_that_line() {
        let file = parse_unified(DIFF).remove(0);
        let lines = &file.hunks[0].lines;
        let rows = diff_view::split_rows(lines);
        // The pure mapping `hunk_rows` uses to turn a pressed half into the line that is selected and later commented on.
        let anchor = |row: usize, half| split_pos(&rows[row], half).map(|at| LineRef { old: lines[at].old, new: lines[at].new });
        let unified = |at: usize| LineRef { old: lines[at].old, new: lines[at].new };
        // A context line is the same line on both halves.
        assert_eq!(anchor(0, Half::Left), Some(unified(0)));
        assert_eq!(anchor(0, Half::Right), Some(unified(0)));
        // A replaced pair: the left half is the removed line (old number only), the right the added line (new number only).
        let (left, right) = (anchor(1, Half::Left).expect("left"), anchor(1, Half::Right).expect("right"));
        assert!(left.old.is_some() && left.new.is_none(), "{left:?}");
        assert!(right.new.is_some() && right.old.is_none(), "{right:?}");
        // A removed run longer than the added one leaves the right half of its last row empty: nothing to anchor there.
        assert!(anchor(2, Half::Left).is_some_and(|l| l.old.is_some() && l.new.is_none()));
        assert_eq!(anchor(2, Half::Right), None);
        // Removed-only anchors old, added-only anchors new.
        let removed = rows.iter().position(|r| r.right.is_none() && r.left.is_some_and(|l| lines[l].text == "gone")).expect("a removed-only row");
        assert!(anchor(removed, Half::Left).is_some_and(|l| l.old.is_some() && l.new.is_none()));
        assert_eq!(anchor(removed, Half::Right), None);
        let added = rows.iter().position(|r| r.left.is_none() && r.right.is_some_and(|l| lines[l].text == "fresh")).expect("an added-only row");
        assert_eq!(anchor(added, Half::Left), None);
        assert!(anchor(added, Half::Right).is_some_and(|l| l.new.is_some() && l.old.is_none()));
    }
}

fn hunk_rows(ui: &mut Ui, rows: &HunkRows<'_>, visible: &diff_view::Visible, under: &mut UnderRow<'_>, acts: &mut Vec<Act>) {
    let plain: Vec<DiffLine> = visible.lines.iter().map(|s| s.line.clone()).collect();
    let marks = if rows.layout.word_highlight { diff_view::changed_words(&plain) } else { Vec::new() };
    let marks_of = |pos: usize| marks.get(pos).map_or(&[][..], Vec::as_slice);
    let selection = under.selection;
    let selected = |index: usize| selection.is_some_and(|s| s.contains(rows.file, rows.hunk, index));
    let select =
        |ui: &Ui, index: usize| Act::Select { file: rows.file, hunk: rows.hunk, line: index, extend: ui.input(|input| input.modifiers.shift) };
    if !rows.split {
        for (pos, shown) in visible.lines.iter().enumerate() {
            let style = CodeStyle { marks: marks_of(pos), wrap: rows.layout.wrap };
            let row = components::diff_row(ui, under.width, &shown.line, selected(shown.index), true, style);
            if row.clicked() {
                acts.push(select(ui, shown.index));
            }
            let here = |extend: bool| Act::Select { file: rows.file, hunk: rows.hunk, line: shown.index, extend };
            match components::gutter_press(ui, &row) {
                // The comment button of a hovered line: a click comments on the line, a drag selects the lines it passes over.
                Some(Gutter::Click) => acts.extend([here(false), Act::OpenComposer]),
                Some(Gutter::DragStart) => {
                    acts.push(here(false));
                    ui.ctx().data_mut(|d| d.insert_temp(drag_key(), Some((rows.file, rows.hunk))));
                }
                Some(Gutter::Menu) => {
                    let at = ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| row.rect.center());
                    acts.push(Act::LinesMenu { file: rows.file, hunk: rows.hunk, line: shown.index, at, restore: Some(row.id) });
                }
                None => {}
            }
            if dragging(ui) == Some((rows.file, rows.hunk)) && components::pointer_over(&row) {
                acts.push(here(true));
            }
            let by_key = row.has_focus() && keymap::pressed(ui, "app.context_menu");
            if row.secondary_clicked() || by_key {
                let at = if by_key { menu::below(row.rect) } else { ui.input(|i| i.pointer.interact_pos()).unwrap_or_else(|| row.rect.center()) };
                acts.push(Act::LinesMenu { file: rows.file, hunk: rows.hunk, line: shown.index, at, restore: Some(row.id) });
            }
            under.draw(ui, rows, &[(shown.index, &shown.line)], acts);
        }
        return;
    }
    for row in diff_view::split_rows(&plain) {
        let side = |pos: Option<usize>| pos.and_then(|p| visible.lines.get(p).map(|s| (p, s)));
        let (left, right) = (side(row.left), side(row.right));
        let cells = [left, right].map(|c| c.map(|(p, s)| Cell { line: &s.line, selected: selected(s.index), marks: marks_of(p) }));
        let out = components::split_row(ui, under.width, cells, rows.layout.wrap, true);
        let index_of = |half: Half| split_pos(&row, half).and_then(|p| visible.lines.get(p)).map(|s| s.index);
        let (file, hunk) = (rows.file, rows.hunk);
        let select_at = |line: usize, extend: bool| Act::Select { file, hunk, line, extend };
        if let Some(index) = out.clicked.and_then(index_of) {
            acts.push(select(ui, index));
        }
        if let Some((half, press)) = out.gutter
            && let Some(index) = index_of(half)
        {
            match press {
                Gutter::Click => acts.extend([select_at(index, false), Act::OpenComposer]),
                Gutter::DragStart => {
                    acts.push(select_at(index, false));
                    ui.ctx().data_mut(|d| d.insert_temp(drag_key(), Some((file, hunk))));
                }
                Gutter::Menu => {
                    let at = ui.input(|i| i.pointer.interact_pos()).unwrap_or_default();
                    acts.push(Act::LinesMenu { file, hunk, line: index, at, restore: None });
                }
            }
        }
        if dragging(ui) == Some((file, hunk))
            && let Some(index) = out.over.and_then(index_of)
        {
            acts.push(select_at(index, true));
        }
        if let Some((half, at, restore)) = out.menu
            && let Some(index) = index_of(half)
        {
            acts.push(Act::LinesMenu { file, hunk, line: index, at, restore: Some(restore) });
        }
        let mut here: Vec<(usize, &DiffLine)> = Vec::new();
        for (_, shown) in [left, right].into_iter().flatten() {
            if here.iter().all(|(index, _)| *index != shown.index) {
                here.push((shown.index, &shown.line));
            }
        }
        under.draw(ui, rows, &here, acts);
    }
}

/// What the whole-stack view draws besides the files.
struct WholeView<'a> {
    omitted: &'a [String],
    unfolded: &'a HashSet<String>,
    /// The file and hunk J K N P last moved to, and what to focus there this frame.
    at: (usize, usize),
    focus: Focus,
    forge: &'static str,
    /// The forge's page for this comparison.
    web_url: Option<&'a str>,
    /// The width of the diff pane; rows and bands are at least this wide.
    width: f32,
}

/// A file of the whole-stack diff: the same lines as a layer's diff, with nothing to select, comment on or mark viewed.
/// The file and hunk headers take focus so J K N P can walk them (rule 21).
fn whole_file_ui(ui: &mut Ui, i: usize, file: &FileDiff, view: &WholeView, acts: &mut Vec<Act>) {
    let renamed = file.old_path.as_deref().map(|old| format!("{old} → ")).unwrap_or_default();
    let p = theme::pal(ui);
    let header = egui::Frame::new().fill(p.sunken).inner_margin(Margin::symmetric(16, 6)).show(ui, |ui| {
        ui.set_width(view.width - 32.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let name = RichText::new(format!("{renamed}{}", file.path)).font(FontId::monospace(theme::sz(ui).code)).color(p.graphite);
            let header = ui.add(egui::Button::new(name).frame(false));
            if view.focus == Focus::File && view.at.0 == i {
                header.request_focus();
                header.scroll_to_me(Some(Align::TOP));
            }
            components::mono_label(ui, format!("+{} −{}", file.additions, file.deletions), 11.5, p.text_secondary);
        });
    });
    components::seam_bottom(ui, header.response.rect);
    if view.omitted.contains(&file.path) {
        // Rule 15: a file the forge left out is not an empty change.
        let note = match view.web_url {
            Some(_) => format!("Too large to show here. Open on {}.", view.forge),
            None => "Too large to show here.".to_string(),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(note).size(theme::sz(ui).body).color(theme::pal(ui).ember_text));
            if let Some(url) = view.web_url
                && ui.button(format!("Open on {}", view.forge)).clicked()
            {
                acts.push(Act::OpenUrl(url.to_string()));
            }
        });
        return;
    }
    if !shows_hunks(file, false, view.unfolded) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(fold_reason(file, false)).size(theme::sz(ui).body).color(theme::pal(ui).text_tertiary));
            if !file.binary && ui.button("Show").clicked() {
                acts.push(Act::Unfold(file.path.clone()));
            }
        });
        return;
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    for (h, hunk) in file.hunks.iter().enumerate() {
        let header = components::hunk_header(ui, view.width, &hunk.header);
        if view.focus == Focus::Hunk && view.at == (i, h) {
            header.request_focus();
            header.scroll_to_me(Some(Align::TOP));
        }
        for line in &hunk.lines {
            components::diff_row(ui, view.width, line, false, false, CodeStyle::default());
        }
    }
}

/// Whether a file's hunks are drawn: not binary, and not folded unless the reader unfolded it.
fn shows_hunks(file: &FileDiff, viewed: bool, unfolded: &HashSet<String>) -> bool {
    let folded = viewed || file.generated || file.additions + file.deletions > LARGE_FILE_LINES;
    !file.binary && (!folded || unfolded.contains(&file.path))
}

/// The stop after (or before) `here`, as `(file, hunk)`. `shown` lists file indices in display order and
/// `hunk_counts` how many hunks each draws: J K step over hunks of files that draw some, N P over every shown file.
/// From a folded file (just marked viewed, say), it goes to the nearest stop in that direction.
fn next_stop(shown: &[usize], hunk_counts: &dyn Fn(usize) -> usize, here: (usize, usize), hunks: bool, forward: bool) -> Option<(usize, usize)> {
    let mut stops = Vec::new();
    for &i in shown {
        let count = if hunks { hunk_counts(i) } else { 0 };
        if hunks && count == 0 {
            continue;
        }
        stops.extend((0..count.max(1)).map(|h| (i, h)));
    }
    let at = stops.iter().position(|&s| s == (here.0, if hunks { here.1 } else { 0 }));
    let rank = |file: usize| shown.iter().position(|&v| v == file);
    let place = rank(here.0);
    match (at, forward) {
        (None, true) => stops.iter().find(|s| rank(s.0) > place),
        (None, false) => stops.iter().rev().find(|s| rank(s.0) < place),
        (Some(p), true) => stops.get(p + 1).or(stops.get(p)),
        (Some(p), false) => stops.get(p.saturating_sub(1)),
    }
    .copied()
}

/// An AI draft under its line or in the AI drafts list: who suggested it, that it is not posted, and what to do with it.
/// The one A and X act on is marked with ▸, so the mark is not only a colour.
fn ai_card(ui: &mut Ui, inline: Option<f32>, draft: &AiDraft, place: Option<String>, current: bool, focus: bool, acts: &mut Vec<Act>) {
    let marker = if current { "▸ " } else { "" };
    let who = format!("{marker}Suggested by {}", draft.finding.agent);
    let mut body = |ui: &mut Ui| {
        if let Some(place) = &place {
            components::note(ui, place.as_str(), theme::pal(ui).text_tertiary);
        }
        components::body_text(ui, &draft.finding.body);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            let keep = Pill::new(PillKind::Primary, "Keep as draft").key(ui, "review.keep_ai").small().show(ui);
            if focus {
                keep.request_focus();
                keep.scroll_to_me(Some(Align::Center));
            }
            if keep.clicked() {
                acts.push(Act::KeepAi { id: Some(draft.id.clone()), edit: false });
            }
            if Pill::new(PillKind::Secondary, "Edit first").key(ui, "review.keep_ai_edit").small().show(ui).clicked() {
                acts.push(Act::KeepAi { id: Some(draft.id.clone()), edit: true });
            }
            if Pill::new(PillKind::Quiet, "Discard").key(ui, "review.discard_ai").small().show(ui).clicked() {
                acts.push(Act::DiscardAi(Some(draft.id.clone())));
            }
        });
    };
    if let Some(width) = inline {
        components::comment_card(ui, width, CardKind::Ai, &who, "AI draft · not posted", body);
    } else {
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                components::label(ui, who.as_str(), 13.0, theme::pal(ui).graphite);
                components::tag(ui, TagKind::OnTheWay, "AI draft · not posted");
            });
            body(ui);
        });
    }
}

/// Where a draft sits, for the Your review list: `invoicer.go:42`, `retry.svg · rendered` for one written on a rendered SVG,
/// or "General" when it has no line to show.
fn draft_place(draft: &Draft) -> String {
    if draft.on_rendered {
        format!("{} · rendered", draft.path.rsplit('/').next().unwrap_or(&draft.path))
    } else if draft.end.shown() == 0 {
        "General".to_string()
    } else {
        draft.location()
    }
}

/// Where a draft or the comment box is drawn.
#[derive(Clone, Copy)]
enum Place {
    /// Between two lines of the diff, `width` wide.
    Line(f32),
    /// On its own under a rendered SVG (frame 79), `width` wide.
    Band(f32),
    /// In Your review.
    Panel,
}

fn draft_tag(draft: &Draft) -> &'static str {
    if draft.on_rendered { "Draft · on the rendered view" } else { "Draft · on this Mac" }
}

/// A draft under its line, under a rendered SVG or in Your review: its text, or its editor, with Edit and Delete. Under an SVG
/// it has the quiet Edit of frame 79 with its key; Delete is in the editor.
fn draft_card(ui: &mut Ui, place: Place, draft: &Draft, editing: &mut Option<(u64, String)>, focus: bool, acts: &mut Vec<Act>) {
    let mut body = |ui: &mut Ui| match editing {
        Some((id, text)) if *id == draft.id => {
            let field = ui.add(TextEdit::multiline(text).desired_width(ui.available_width()));
            let name = format!("Edit draft {}", draft_place(draft));
            ui.ctx().accesskit_node_builder(field.id, |node| node.set_label(name));
            if focus {
                field.request_focus();
                field.scroll_to_me(Some(Align::Center));
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                if Pill::new(PillKind::Primary, "Save").key(ui, "review.submit").small().show(ui).clicked() {
                    acts.push(Act::SaveEdit);
                }
                if Pill::new(PillKind::Secondary, "Cancel").key(ui, "review.back").small().show(ui).clicked() {
                    acts.push(Act::CancelEdit);
                }
                if Pill::new(PillKind::Danger, "Delete draft").small().show(ui).clicked() {
                    acts.push(Act::DeleteDraft(draft.id));
                }
            });
        }
        _ => {
            components::body_text(ui, &draft.body);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                let edit = match place {
                    Place::Band(_) => Pill::new(PillKind::Quiet, "Edit").key(ui, "review.edit_draft").small().show(ui),
                    Place::Line(_) | Place::Panel => Pill::new(PillKind::Secondary, "Edit").small().show(ui),
                };
                if focus {
                    edit.request_focus();
                    edit.scroll_to_me(Some(Align::Center));
                }
                if edit.clicked() {
                    acts.push(Act::StartEdit(draft.id));
                }
                if !matches!(place, Place::Band(_)) && Pill::new(PillKind::Danger, "Delete draft").small().show(ui).clicked() {
                    acts.push(Act::DeleteDraft(draft.id));
                }
            });
        }
    };
    match place {
        Place::Line(width) => components::comment_card(ui, width, CardKind::Draft, "You", draft_tag(draft), body),
        Place::Band(width) => components::loose_comment_card(ui, width, CardKind::Draft, "You", draft_tag(draft), body),
        Place::Panel => card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                components::label(ui, "You", 13.0, theme::pal(ui).graphite);
                components::tag(ui, TagKind::OnTheWay, draft_tag(draft));
            });
            components::mono_label(ui, draft_place(draft), 11.0, theme::pal(ui).text_tertiary);
            body(ui);
        }),
    }
}

pub(crate) fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    components::card_frame(ui).show(ui, add);
}

/// The comment box under the selected lines (frame 05), or under a rendered SVG.
fn composer_ui(ui: &mut Ui, place: Place, composer: &mut Option<String>, focus: bool, title: String, acts: &mut Vec<Act>) {
    let Some(body) = composer else { return };
    let add = |ui: &mut Ui| {
        components::label(ui, title, 13.0, theme::pal(ui).graphite);
        let field = ui.add(TextEdit::multiline(body).desired_width(f32::INFINITY).hint_text("Draft · on this Mac until you submit"));
        if focus {
            field.request_focus();
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            if Pill::new(PillKind::Primary, "Save draft").key(ui, "review.submit").small().show(ui).clicked() {
                acts.push(Act::SaveComposer);
            }
            if Pill::new(PillKind::Secondary, "Cancel").key(ui, "review.back").small().show(ui).clicked() {
                acts.push(Act::CancelComposer);
            }
        });
    };
    match place {
        Place::Band(width) => components::loose_comment_card(ui, width, CardKind::Draft, "You", "Draft · on the rendered view", add),
        Place::Line(width) => components::comment_card(ui, width, CardKind::Draft, "You", "Draft · on this Mac", add),
        Place::Panel => card(ui, add),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn j_and_k_walk_hunks_across_files_and_n_p_walk_files() {
        // Three files with 2, 0 (folded) and 1 hunks.
        let counts = |i: usize| [2, 0, 1].get(i).copied().unwrap_or(0);
        let shown = [0, 1, 2];
        assert_eq!(next_stop(&shown, &counts, (0, 0), true, true), Some((0, 1)));
        assert_eq!(next_stop(&shown, &counts, (0, 1), true, true), Some((2, 0)));
        assert_eq!(next_stop(&shown, &counts, (2, 0), true, false), Some((0, 1)));
        assert_eq!(next_stop(&shown, &counts, (0, 1), false, true), Some((1, 0)));
        assert_eq!(next_stop(&shown, &counts, (2, 0), false, true), Some((2, 0)));
        // From the folded file, J goes to the next stop after it.
        assert_eq!(next_stop(&shown, &counts, (1, 0), true, true), Some((2, 0)));
    }
}

/// The "Since your review" summary: your review and its age, then what is compared and how big it is.
fn since_line(since: &SinceLast, now: DateTime<Utc>) -> String {
    let (adds, dels) = since.files.iter().fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
    let short = |sha: &str| sha.get(..7).unwrap_or(sha).to_string();
    let when = since.review.submitted_at.map(|at| format!(" {} ago", crate::age(now, at))).unwrap_or_default();
    let size = format!("+{adds} −{dels} in {}", components::files_word(since.files.len()));
    let you = since.review.state.you();
    if since.rewritten {
        return format!("{you}{when} · its commit {} was force-pushed away · whole change, {size}", short(&since.from));
    }
    let commits = if since.commits_since == 1 { "1 commit".to_string() } else { format!("{} commits", since.commits_since) };
    format!("{you}{when} · {commits} since, {} → {} · {size}", short(&since.from), short(&since.to))
}

#[cfg(test)]
mod since_tests {
    use pr_craft_core::turn::{MyReview, ReviewState};

    use super::*;

    // An invented result shaped like `review.since_last`.
    fn since(rewritten: bool) -> SinceLast {
        SinceLast {
            change: ChangeRef { forge: pr_craft_core::Forge::GitHub, repo: "acme/api".into(), number: 1, host: None },
            review: MyReview { state: ReviewState::ChangesRequested, submitted_at: None, commit: Some("aaaaaaaaaa".into()) },
            from: "aaaaaaaaaa".into(),
            to: "bbbbbbbbbb".into(),
            commits_since: 7,
            files: Vec::new(),
            omitted: Vec::new(),
            truncated: false,
            rewritten,
            web_url: None,
        }
    }

    #[test]
    fn a_rewritten_history_title_does_not_claim_a_count_of_new_commits_or_a_range() {
        let (normal, rewritten) = (since_line(&since(false), Utc::now()), since_line(&since(true), Utc::now()));
        assert_ne!(normal, rewritten);
        assert!(normal.contains('7') && normal.contains("bbbbbbb"), "{normal}");
        assert!(!rewritten.contains('7'), "no count of new commits: {rewritten}");
        assert!(!rewritten.contains("bbbbbbb"), "no from-to range: {rewritten}");
    }
}

#[cfg(test)]
mod block_tests {
    use super::*;

    fn target(file: usize) -> Option<BlockTarget> {
        Some(BlockTarget { file, item: 3, line: LineRef { old: None, new: Some(9) } })
    }

    #[test]
    fn a_block_comment_box_does_not_outlive_its_file_or_its_rendered_view() {
        let (mut block, mut composer) = (target(2), Some("why?".to_string()));
        reconcile_block(&mut block, &mut composer, 2, true);
        assert!(block.is_some() && composer.is_some(), "still on its file, still rendered");
        reconcile_block(&mut block, &mut composer, 5, true);
        assert!(block.is_none() && composer.is_none(), "another file is current");
        let (mut block, mut composer) = (target(2), Some("why?".to_string()));
        reconcile_block(&mut block, &mut composer, 2, false);
        assert!(block.is_none() && composer.is_none(), "its file is no longer drawn rendered");
    }

    #[test]
    fn a_source_comment_never_inherits_a_block_target() {
        let (mut block, mut composer) = (target(1), Some("text".to_string()));
        drop_block(&mut block, &mut composer);
        assert!(block.is_none() && composer.is_none());
        // With no block target a composer belongs to the source view and is left alone.
        let (mut block, mut composer) = (None, Some("source comment".to_string()));
        reconcile_block(&mut block, &mut composer, 0, false);
        assert_eq!(composer.as_deref(), Some("source comment"));
    }

    #[test]
    fn a_comment_on_a_block_in_another_file_makes_that_file_current_so_its_box_survives_the_next_frame() {
        // The reader is on file 0, hunk 2, and presses a block's comment button in file 3.
        let (mut file, mut hunk) = (0, 2);
        let (mut block, mut composer) = (None, None);
        make_current(&mut file, &mut hunk, 3);
        block = block.or(target(3));
        composer = composer.or(Some(String::new()));
        assert_eq!((file, hunk), (3, 0));
        // The next frame reconciles against the current file, as `ui()` does.
        reconcile_block(&mut block, &mut composer, file, true);
        assert!(block.is_some() && composer.is_some(), "the box stays open");
        // Pressing it on the file already current keeps the hunk the reader was on.
        let mut hunk = 1;
        make_current(&mut file, &mut hunk, 3);
        assert_eq!(hunk, 1);
    }
}

#[cfg(test)]
mod ai_tests {
    use super::*;
    use crate::ai::tests::{draft, drawn};

    #[test]
    fn an_ai_draft_says_it_is_not_posted_and_marks_the_one_the_keys_act_on() {
        let finding = draft("r/0", "a.go", Some(3), None, DraftState::Pending);
        let mut acts = Vec::new();
        let current = drawn(&Context::default(), |ui| ai_card(ui, None, &finding, Some("a.go:3".into()), true, false, &mut acts));
        let other = drawn(&Context::default(), |ui| ai_card(ui, None, &finding, Some("a.go:3".into()), false, false, &mut acts));
        assert!(current.contains("not posted") && current.contains(&finding.finding.body), "{current}");
        assert_ne!(current, other, "the card the keys act on is marked in words, not only by colour");
        assert!(acts.is_empty(), "drawing a card keeps and discards nothing");
    }
}
