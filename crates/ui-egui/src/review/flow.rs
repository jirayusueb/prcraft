use super::{
    Act, AiSheet, BannerKind, ChangeRef, CommitAct, Context, Fetch, FileStatus, Focus, LocalAct, LocalReview, Margin, Outcome, Ready, Receiver,
    ReviewThread, RichText, Runs, Selection, ShareSheet, Target, TextEdit, ThreadAct, TryRecvError, Ui, UnderRow, WAIT_FOR_FORGE, activity, card,
    commits_engine, components, drop_block, egui, engine, file_view, keymap, make_current, menu, mpsc, parent_dir, pick, theme, thread_engine,
    threads, turn,
};

/// A reply or a resolve on its way to the forge.
pub(super) struct ThreadWrite {
    pub(super) rx: Receiver<Result<String, String>>,
    pub(super) thread: String,
    /// A reply: its text leaves the field once the forge has it.
    pub(super) reply: bool,
}

/// The threads that sit on no line of the current diff (outdated, or the forge placed them nowhere), after the file's last hunk.
pub(super) fn loose_threads(ui: &mut Ui, under: &mut UnderRow<'_>, acts: &mut Vec<Act>) {
    let mut asked = Vec::new();
    for thread in under.threads.iter().filter(|t| !t.placed()) {
        threads::thread_ui(ui, under.width, under.forge, thread, under.cards, false, &mut asked);
    }
    acts.extend(asked.into_iter().map(Act::Thread));
}

impl Ready {
    pub(super) fn keys(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
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
            ("review.next_outdated", Act::StepOutdated),
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

    pub(super) fn poll_handoff(&mut self) {
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
    pub(super) fn send_handoff(&mut self, ctx: &Context) {
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
    pub(super) fn handoff_sheet(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
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

    pub(super) fn poll_submit(&mut self) {
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

    pub(super) fn set_local(&mut self, local: LocalReview) {
        // Text typed into the summary since the last save stays; only an unedited field follows the file.
        if self.body == self.view.local.body {
            self.body.clone_from(&local.body);
        }
        self.view.local = local;
        self.recheck_outdated();
    }

    pub(super) fn recheck_outdated(&mut self) {
        self.outdated = self.view.anchor.outdated_drafts(&self.view.local, &self.view.files);
        if self.outdated.is_empty() {
            self.confirm_outdated = false;
        }
    }

    pub(super) fn apply(&mut self, ctx: &Context, acts: Vec<Act>, runs: &mut Runs) -> Outcome {
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
                    self.step_draft(forward, false);
                    Ok(())
                }
                Act::StepOutdated => {
                    self.step_draft(true, true);
                    Ok(())
                }
                Act::ConfirmOutdated => {
                    self.confirm_outdated = false;
                    self.submit_with(ctx, true, true)
                }
                Act::DismissOutdated => {
                    self.confirm_outdated = false;
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
                    self.submit_with(ctx, true, false)
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
                Act::OpenUrl(url, path) => {
                    crate::open_on_forge(ctx, &url, "review", &self.view.change, Some(&path));
                    Ok(())
                }
                Act::OpenOnForge(path) => {
                    crate::open_on_forge(ctx, &self.view.meta.url, "review", &self.view.change, path.as_deref());
                    Ok(())
                }
            };
            if let Err(error) = result {
                self.error = Some(crate::fail(error));
            }
        }
        Outcome::Stay
    }

    pub(super) fn submit(&mut self, ctx: &Context) -> Result<(), pr_craft_engine::CommandError> {
        self.submit_with(ctx, false, false)
    }

    /// Posts the review. After a post that ran out of time, it first asks (`confirmed` is the reader's yes), because that post may
    /// already have gone through and posting again could post twice. Drafts on changed lines ask a second time (`outdated_ok`).
    pub(super) fn submit_with(&mut self, ctx: &Context, confirmed: bool, outdated_ok: bool) -> Result<(), pr_craft_engine::CommandError> {
        if self.submitting.is_some() {
            return Ok(());
        }
        let local = engine::compose(&self.view.change, self.view.local.verdict, self.body.clone())?;
        self.set_local(local);
        if !confirmed && self.view.local.post_unknown.is_some() {
            self.confirm_post = true;
            return Ok(());
        }
        if !outdated_ok && !self.outdated.is_empty() {
            self.confirm_outdated = true;
            return Ok(());
        }
        self.error = None;
        self.notice = None;
        let (tx, rx) = mpsc::channel();
        let (ctx, change) = (ctx.clone(), self.view.change.clone());
        std::thread::spawn(move || {
            let _ = tx.send(engine::submit(&change, confirmed, outdated_ok).map_err(crate::fail));
            ctx.request_repaint();
        });
        self.submitting = Some(rx);
        Ok(())
    }

    /// Starts reading the change's commits.
    pub(super) fn start_commits(&mut self, ctx: &Context) {
        let change = self.view.change.clone();
        self.commits.list = Fetch::start(ctx, move || commits_engine::list(&change).map_err(crate::fail));
    }

    pub(super) fn commit_act(&mut self, ctx: &Context, act: CommitAct) {
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
    pub(super) fn say_merges(&mut self) {
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
    pub(super) fn load_range(&mut self, ctx: &Context) {
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
    pub(super) fn commits_popover(&mut self, ctx: &Context, acts: &mut Vec<Act>) {
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
    pub(super) fn local_popover(&mut self, ctx: &Context, acts: &mut Vec<Act>) {
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
    pub(super) fn open_target(&self) -> Option<Target> {
        let file = self.current_file().filter(|f| f.status != FileStatus::Deleted).filter(|_| !self.readonly())?;
        let selected = self.selection.filter(|s| s.file == self.file);
        let hunk = file.hunks.get(selected.map_or(self.hunk, |s| s.hunk))?;
        let from = selected.map_or(0, |s| s.head);
        // A removed line has no number in the head, so the nearest line at or after it, else before it, names the place.
        let line = hunk.lines.iter().skip(from).chain(hunk.lines.iter().take(from).rev()).find_map(|l| l.new)?;
        Some(Target { path: file.path.clone(), line })
    }

    /// Why the threads are not all on screen: still being read, failed (never "no threads"), or only part of them read.
    pub(super) fn threads_note(&self, ui: &mut Ui) {
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

    /// Whether file `i` is drawn as hunks the reader can see its threads in: not folded, not shown rendered, not in a read-only view.
    pub(super) fn thread_drawn(&self, i: usize) -> bool {
        !self.readonly() && self.view.files.get(i).is_some_and(|f| self.expanded(f) && !self.drawn_rendered(f))
    }

    /// The review threads of the files on screen in file and line order, as `(file, line, id)`. A file the filter hides has none.
    pub(super) fn thread_order(&self) -> Vec<(usize, u32, String)> {
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

    pub(super) fn thread_by_id(&self, id: &str) -> Option<&ReviewThread> {
        match &self.threads {
            Fetch::Ready(list) => list.threads.iter().find(|t| t.id == id),
            _ => None,
        }
    }

    /// T and ⇧T: the next or previous thread becomes the one ⇧R and ⌥R act on, and its Resolve button takes focus.
    pub(super) fn step_thread(&mut self, forward: bool) {
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
    pub(super) fn current_thread(&self) -> Option<String> {
        let id = self.cards.current.as_deref()?;
        let order = self.thread_order();
        order.iter().find(|o| o.2 == id && o.0 == self.file && self.visible.contains(&o.0)).map(|o| o.2.clone())
    }

    /// The file of the stepped thread, for the tally of an open on the forge.
    pub(super) fn thread_path(&self) -> Option<String> {
        let (Fetch::Ready(list), Some(id)) = (&self.threads, self.cards.current.as_deref()) else { return None };
        list.threads.iter().find(|t| t.id == id).map(|t| t.path.clone())
    }

    /// Forgets the stepped thread once the reader is on another file or the filter hides its file.
    pub(super) fn drop_stale_thread(&mut self) {
        if self.cards.current.is_some() && self.current_thread().is_none() {
            self.cards.current = None;
        }
    }

    pub(super) fn resolve_current(&mut self, ctx: &Context) {
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

    pub(super) fn reply_current(&mut self) {
        if let Some(id) = self.current_thread() {
            self.cards.open.insert(id.clone());
            self.cards.focus = Some((id, threads::Focus::Reply));
        }
    }

    /// The review holds a draft on the line of this thread, so a reply now and the draft's post would both land there.
    pub(super) fn overlaps_draft(&self, id: &str) -> bool {
        let Some(thread) = self.thread_by_id(id) else { return false };
        let Some(line) = thread.line else { return false };
        self.view.local.drafts.iter().any(|d| d.path == thread.path && d.end.shown() == line.shown())
    }

    pub(super) fn thread_act(&mut self, ctx: &Context, act: ThreadAct) {
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
    pub(super) fn write_reply(&mut self, ctx: &Context, id: String) {
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
    pub(super) fn write_resolve(&mut self, ctx: &Context, id: String) {
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

    pub(super) fn start_write(
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
    pub(super) fn poll_write(&mut self, ctx: &Context) {
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
}
