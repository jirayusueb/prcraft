use super::{
    Act, AiDraft, Align, BannerKind, CardKind, Draft, DraftState, Layout, LineRef, NewDraft, PanelAct, Pill, PillKind, Ready, Rect, Runs, ScrollArea,
    TagKind, TextEdit, Ui, UiBuilder, Verdict, activity, ai, components, egui, engine, keymap, theme, verdict_note,
};

/// Closes the comment box of a rendered block.
pub(super) fn drop_block(block: &mut Option<BlockTarget>, composer: &mut Option<String>) {
    if block.take().is_some() {
        *composer = None;
    }
}

/// Closes a block's comment box when it no longer belongs on screen: another file is current, or its file is not drawn rendered.
pub(super) fn reconcile_block(block: &mut Option<BlockTarget>, composer: &mut Option<String>, file: usize, rendered_here: bool) {
    if block.is_some_and(|b| b.file != file || !rendered_here) {
        drop_block(block, composer);
    }
}

/// Makes `to` the current file, at its first hunk, when another file was.
pub(super) fn make_current(file: &mut usize, hunk: &mut usize, to: usize) {
    if *file != to {
        (*file, *hunk) = (to, 0);
    }
}

/// A comment being written on a rendered Markdown block: the file, the block and the source line it posts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockTarget {
    pub(super) file: usize,
    pub(super) item: usize,
    pub(super) line: LineRef,
}

/// An AI draft under its line or in the AI drafts list: who suggested it, that it is not posted, and what to do with it.
/// The one A and X act on is marked with ▸, so the mark is not only a colour.
pub(super) fn ai_card(ui: &mut Ui, inline: Option<f32>, draft: &AiDraft, place: Option<String>, current: bool, focus: bool, acts: &mut Vec<Act>) {
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
pub(super) fn draft_place(draft: &Draft) -> String {
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
pub(super) enum Place {
    /// Between two lines of the diff, `width` wide.
    Line(f32),
    /// On its own under a rendered SVG (frame 79), `width` wide.
    Band(f32),
    /// In Your review.
    Panel,
}

pub(super) fn draft_tag(draft: &Draft) -> &'static str {
    if draft.on_rendered { "Draft · on the rendered view" } else { "Draft · on this Mac" }
}

/// A draft under its line, under a rendered SVG or in Your review: its text, or its editor, with Edit and Delete. Under an SVG
/// it has the quiet Edit of frame 79 with its key; Delete is in the editor.
pub(super) fn draft_card(ui: &mut Ui, place: Place, draft: &Draft, editing: &mut Option<(u64, String)>, focus: bool, acts: &mut Vec<Act>) {
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
pub(super) fn composer_ui(ui: &mut Ui, place: Place, composer: &mut Option<String>, focus: bool, title: String, acts: &mut Vec<Act>) {
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

impl Ready {
    /// The AI draft A, ⇧A and X act on: the one stepped to (⌥↓), else the one under the selected lines or in the current
    /// hunk, else the first draft read at the commit on screen. Never a draft of an older commit by default.
    pub(super) fn ai_current(&self) -> Option<&ai::Pending> {
        let at = ai::pick_current(&self.ai_pending, self.ai_focus.as_deref(), self.ai_under_cursor())?;
        self.ai_pending.get(at)
    }

    /// The id of the placed AI draft on the selected lines, or else in the current hunk.
    pub(super) fn ai_under_cursor(&self) -> Option<&str> {
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
    pub(super) fn keep_ai(&mut self, runs: &mut Runs, id: Option<String>, edit: bool) {
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

    pub(super) fn discard_ai(&mut self, runs: &mut Runs, id: Option<String>) {
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
    pub(super) fn step_ai(&mut self, forward: bool) {
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
    pub(super) fn ai_panel(&self, ui: &mut Ui, acts: &mut Vec<Act>) {
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
    pub(super) fn ai_keys(&self, ui: &Ui, typing: bool, acts: &mut Vec<Act>) {
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

    pub(super) fn review_pane(&mut self, ui: &mut Ui, acts: &mut Vec<Act>, runs: &mut Runs) {
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

    pub(super) fn drafts_pane(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
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
        self.outdated_note(ui, acts);
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
    pub(super) fn post_unknown_note(&self, ui: &mut Ui, acts: &mut Vec<Act>) {
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

    /// "N drafts · M on changed lines", a live region, with the key that steps to them; once Submit was pressed, the extra confirm.
    pub(super) fn outdated_line(&self, ui: &mut Ui) {
        let drafts = self.view.local.drafts.len();
        if drafts == 0 {
            return;
        }
        let p = theme::pal(ui);
        let mut line = format!("{drafts} {} · {} on changed lines", if drafts == 1 { "draft" } else { "drafts" }, self.outdated.len());
        let color = if self.outdated.is_empty() { p.text_tertiary } else { p.text_secondary };
        if !self.outdated.is_empty() {
            line = keymap::hint(ui, &line, "review.next_outdated");
        }
        let said = components::note(ui, line, color);
        ui.ctx().accesskit_node_builder(said.id, |node| node.set_live(egui::accesskit::Live::Polite));
    }

    pub(super) fn outdated_note(&self, ui: &mut Ui, acts: &mut Vec<Act>) {
        if !self.confirm_outdated {
            return;
        }
        let places: Vec<String> = self.view.local.drafts.iter().filter(|d| self.outdated.contains(&d.id)).map(Draft::location).collect();
        let n = places.len();
        let title = format!("{n} {} on lines that changed since you wrote them", if n == 1 { "draft is" } else { "drafts are" });
        ui.add_space(theme::SPACE_2);
        let note = components::banner(ui, BannerKind::Warn, &title, &places.join(", "));
        ui.ctx().accesskit_node_builder(note.id, |node| node.set_live(egui::accesskit::Live::Polite));
        ui.add_space(theme::SPACE_2);
        ui.horizontal(|ui| {
            if Pill::new(PillKind::Primary, "Post anyway").small().show(ui).clicked() {
                acts.push(Act::ConfirmOutdated);
            }
            if Pill::new(PillKind::Secondary, "Cancel").small().show(ui).clicked() {
                acts.push(Act::DismissOutdated);
            }
        });
    }

    /// Moves to the next or previous draft in file order and focuses its card (`[` and `]`); with `outdated`, only among the drafts
    /// on changed lines.
    pub(super) fn step_draft(&mut self, forward: bool, outdated: bool) {
        let rank = |path: &str| self.view.files.iter().position(|f| f.path == path).unwrap_or(usize::MAX);
        let mut order: Vec<(usize, u32, u64)> = self
            .view
            .local
            .drafts
            .iter()
            .filter(|d| !outdated || self.outdated.contains(&d.id))
            .map(|d| (rank(&d.path), d.end.shown(), d.id))
            .collect();
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

    pub(super) fn save_block_comment(&mut self, target: BlockTarget) -> Result<(), pr_craft_engine::CommandError> {
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

    pub(super) fn save_composer(&mut self) -> Result<(), pr_craft_engine::CommandError> {
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
    use craft::egui::Context;
    use pr_craft_core::run::DraftState;

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

#[cfg(test)]
mod outdated_tests {
    use craft::egui::Context;
    use pr_craft_core::review::Anchor;

    use super::*;
    use crate::ai::{Fetch, tests::drawn};

    /// The retry fixture with its draft on line 35 and a second one on line 900, both written on an earlier commit.
    fn moved() -> Ready {
        let mut view = crate::parity::retry_view();
        let mut gone = view.local.drafts[0].clone();
        (gone.id, gone.end.new) = (2, Some(900));
        view.local.drafts.push(gone);
        view.local.anchor = Some(Anchor { head_sha: "old".into(), base_sha: None, start_sha: None });
        Ready::with(view, false, Default::default(), None, Fetch::Idle)
    }

    #[test]
    fn the_line_counts_drafts_and_those_on_changed_lines_and_the_key_steps_to_them() {
        let mut ready = moved();
        let text = drawn(&Context::default(), |ui| ready.outdated_line(ui));
        assert!(text.contains("2 drafts · 1 on changed lines"), "{text}");
        ready.step_draft(true, true);
        assert_eq!(ready.draft_focus, Some(2), "focus goes to the draft on the changed line, not the one in place");
        assert!(ready.focus_draft);
        ready.view.local.drafts.clear();
        ready.recheck_outdated();
        assert!(drawn(&Context::default(), |ui| ready.outdated_line(ui)).is_empty(), "no drafts, no line");
    }

    #[test]
    fn submit_asks_the_extra_confirm_and_lists_the_drafts() {
        let mut ready = moved();
        ready.confirm_outdated = true;
        let mut acts = Vec::new();
        let text = drawn(&Context::default(), |ui| ready.outdated_note(ui, &mut acts));
        assert!(text.contains("1 draft is on lines that changed since you wrote them") && text.contains("Post anyway"), "{text}");
        assert!(acts.is_empty());
    }
}
