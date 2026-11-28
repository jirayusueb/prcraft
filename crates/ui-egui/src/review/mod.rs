//! The review screen: files, the diff, and the review being written (Figma frames 02, 04, 05, 10, 23, 33, 34, 46).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use std::sync::mpsc::{self, Receiver, TryRecvError};

use chrono::{DateTime, Utc};

use craft::egui::{self, Align, Context, FontId, Layout, Margin, Rect, RichText, ScrollArea, TextEdit, Ui, UiBuilder};

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

mod composer;
mod diff;
mod files;
mod flow;
mod header;
pub(crate) use composer::card;
use composer::{BlockTarget, Place, ai_card, composer_ui, draft_card, drop_block, make_current, reconcile_block};
use diff::{Expanding, UnderRow, shows_hunks};
use files::{drag_key, dragging, parent_dir};
use flow::{ThreadWrite, loose_threads};
use header::{back_trail, verdict_note};

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
    /// Open the change on the forge; the path is the file the open was about, when it was one.
    OpenOnForge(Option<String>),
    /// Open this URL, as the forge shows something PR Craft cannot, for this file.
    OpenUrl(String, String),
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
}

/// What a test reads off a review: the current file, the selected lines (text, count), whether the Open locally popover is open,
/// and the viewed mark a folder would set with the files it changes.
#[cfg(test)]
pub(crate) type Probe = (usize, Option<(String, usize)>, bool, (bool, Vec<String>));
