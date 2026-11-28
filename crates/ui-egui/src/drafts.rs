//! The Local drafts queue (Figma frames 01 and 107): every change with something of the reader's on this Mac, from
//! `review.drafts_all`, which reads the saved reviews and asks no forge. Enter opens the review. Rule 15: an empty list says
//! "No drafts on this Mac" only when every saved review was read.

use std::sync::mpsc::{self, TryRecvError};

use chrono::Utc;
use craft::egui::{Context, Margin, ScrollArea, Ui};
use pr_craft_core::ChangeRef;
use pr_craft_core::review::Verdict;
use pr_craft_engine::review::{DraftsAll, Pending};

use crate::components::{self, Pill, PillKind};
use crate::{Inbox, PrCraftApp, age, keymap, theme};

/// What the queue knows: the saved reviews, or why they could not be read.
pub(crate) type Drafts = Result<DraftsAll, String>;

/// The key a row has in the list and in `selected`: the change's reference and host.
pub(crate) fn row_id(change: &ChangeRef) -> String {
    format!("{}{}", change.host.as_deref().unwrap_or(""), change.reference())
}

fn verdict_line(pending: &Pending) -> String {
    let n = pending.drafts;
    let drafts = if n == 1 { "1 draft".to_string() } else { format!("{n} drafts") };
    match pending.verdict {
        Verdict::Comment => drafts,
        other => format!("{drafts} · verdict: {}", other.word()),
    }
}

impl PrCraftApp {
    /// Starts a read of the saved reviews off the UI thread, unless one is on its way. It runs once at start-up, when the queue is
    /// shown and when a review closes; nothing re-reads on a timer. A fixture app never reads this Mac.
    pub(crate) fn refresh_local_drafts(&mut self, ctx: &Context) {
        if !self.drafts_live || self.drafts_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(pr_craft_engine::review::drafts_all().map_err(crate::fail));
            ctx.request_repaint();
        });
        self.drafts_rx = Some(rx);
    }

    /// Takes the answer of a read that is on its way. Call once a frame.
    pub(crate) fn poll_local_drafts(&mut self) {
        let Some(rx) = &self.drafts_rx else { return };
        match rx.try_recv() {
            Ok(read) => {
                self.local_drafts = Some(read);
                self.drafts_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.local_drafts = Some(Err("reading the saved reviews stopped before answering".into()));
                self.drafts_rx = None;
            }
        }
    }

    /// How many changes hold something, for the sidebar: `?` while the saved reviews could not be read.
    pub(crate) fn local_drafts_count(&self) -> String {
        match &self.local_drafts {
            Some(Ok(all)) if all.unreadable.is_empty() => all.changes.len().to_string(),
            Some(Ok(all)) => format!("{}+", all.changes.len()),
            Some(Err(_)) => "?".to_string(),
            None => String::new(),
        }
    }

    /// The change's title when the inbox lists it, else its reference.
    fn draft_title(&self, change: &ChangeRef) -> String {
        match self.changes.iter().find(|c| c.change_ref() == *change) {
            Some(listed) => listed.title.clone(),
            None => change.reference(),
        }
    }

    pub(crate) fn local_drafts_ids(&self) -> Vec<String> {
        match &self.local_drafts {
            Some(Ok(all)) => all.changes.iter().map(|p| row_id(&p.change)).collect(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn local_drafts_list(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        let pad = Margin { left: 20, right: 20, top: 14, bottom: 12 };
        let count = match &self.local_drafts {
            Some(Ok(all)) if all.changes.is_empty() => "none".to_string(),
            Some(Ok(all)) => format!("{} of your changes", all.changes.len()),
            _ => String::new(),
        };
        components::padded(ui, pad, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                components::label(ui, "Local drafts", 16.0, p.graphite);
                let said = components::label(ui, count, 12.0, p.text_tertiary);
                ui.ctx().accesskit_node_builder(said.id, |node| node.set_live(craft::egui::accesskit::Live::Polite));
            });
        });
        components::rule(ui);
        let Some(read) = &self.local_drafts else {
            components::padded(ui, Margin::symmetric(20, 36), |ui| {
                components::empty_state(ui, "Reading the saved reviews", "Drafts on this Mac appear here.", &[])
            });
            return;
        };
        let all = match read {
            Ok(all) => all.clone(),
            Err(error) => {
                let body = format!("{error}. A draft may be saved that is not listed. Nothing was changed.");
                components::padded(ui, Margin::symmetric(20, 36), |ui| {
                    components::empty_state(ui, "The saved reviews could not be read", &body, &[])
                });
                return;
            }
        };
        // Rule 15: a file that did not parse still holds drafts, so it is named above the list, never dropped.
        if !all.unreadable.is_empty() {
            let body = format!("{} could not be read, so this list may be missing changes.", all.unreadable.join("; "));
            components::padded(ui, Margin::symmetric(20, 8), |ui| {
                crate::live_label(ui, body, p.ember_text);
            });
        }
        if all.changes.is_empty() {
            let (title, body) = if all.unreadable.is_empty() {
                (
                    "No drafts on this Mac",
                    "Comments and verdicts you start are kept here, offline too, until you submit the review. Nothing posts on its own.",
                )
            } else {
                ("Nothing listed", "The reviews that could be read hold no drafts. The note above names those that could not.")
            };
            let go = "Go to Review requested";
            let key = keymap::label(ui, "inbox.queue");
            let pressed = components::padded(ui, Margin::symmetric(20, 36), |ui| components::empty_state(ui, title, body, &[(go, key.as_str())]));
            if pressed == Some(0) {
                self.show(Inbox::Queue);
            }
            return;
        }
        let now = Utc::now();
        let (mut clicked, mut opened) = (None, None);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for pending in &all.changes {
                let id = row_id(&pending.change);
                let selected = self.selected.as_deref() == Some(id.as_str());
                let title = self.draft_title(&pending.change);
                let meta = format!("{}  ·  {}", pending.change.reference(), pending.change.forge.name());
                let right = pending.edited.map(|at| format!("edited {} ago", age(now, at))).unwrap_or_default();
                let line = match &pending.post_unknown {
                    Some(_) => format!("{} · a post may have gone through: check the forge before posting again", verdict_line(pending)),
                    None => verdict_line(pending),
                };
                let model = components::ReviewedRow {
                    forge: pending.change.forge,
                    title: &title,
                    meta,
                    line,
                    right,
                    yours: pending.post_unknown.is_some(),
                    selected,
                };
                let row = components::reviewed_row(ui, &id, &model);
                if row.clicked() {
                    clicked = Some(id.clone());
                }
                if row.has_focus() && keymap::pressed(ui, "inbox.open") {
                    opened = Some((pending.change.clone(), title.clone()));
                }
                if selected && self.focus_selected {
                    row.request_focus();
                    row.scroll_to_me(None);
                }
            }
        });
        self.focus_selected = false;
        if clicked.is_some() {
            self.selected = clicked;
        }
        if let Some((change, title)) = opened {
            self.open_review(ui.ctx(), change, title);
        }
    }

    pub(crate) fn local_drafts_detail(&mut self, ui: &mut Ui) {
        let found = match (&self.local_drafts, self.selected.as_deref()) {
            (Some(Ok(all)), Some(id)) => all.changes.iter().find(|p| row_id(&p.change) == id).cloned(),
            _ => None,
        };
        let Some(pending) = found else {
            components::empty_pane(ui, "Nothing selected", "Pick a change to see what is saved for it on this Mac.");
            return;
        };
        let p = theme::pal(ui);
        let title = self.draft_title(&pending.change);
        components::label(ui, pending.change.reference(), 12.0, p.text_tertiary);
        ui.add_space(theme::SPACE_2);
        ui.add(craft::egui::Label::new(craft::egui::RichText::new(&title).size(theme::sz(ui).title).color(p.graphite)).wrap());
        ui.add_space(theme::SPACE_2);
        components::note_sized(
            ui,
            format!("{}. Saved on this Mac; nothing is posted until you submit.", verdict_line(&pending)),
            13.0,
            p.text_secondary,
        );
        if let Some(why) = &pending.post_unknown {
            ui.add_space(theme::SPACE_2);
            components::banner(ui, components::BannerKind::Warn, "A post may already have gone through", why);
        }
        ui.add_space(theme::SPACE_3);
        if Pill::new(PillKind::Primary, "Open review").key(ui, "inbox.open").show(ui).clicked() {
            self.open_review(ui.ctx(), pending.change.clone(), title);
        }
    }
}

/// Test fixture: an app whose saved reviews are `changes` and whose queue is on screen.
#[cfg(test)]
pub(crate) fn fixture_drafts(app: &mut PrCraftApp, changes: Vec<Pending>, unreadable: Vec<String>) {
    app.view = Inbox::LocalDrafts;
    app.local_drafts = Some(Ok(DraftsAll { changes, unreadable }));
}

#[cfg(test)]
pub(crate) fn pending(change: ChangeRef, drafts: usize, verdict: Verdict) -> Pending {
    Pending { change, drafts, verdict, post_unknown: None, edited: Some(Utc::now() - chrono::Duration::hours(2)) }
}
