//! Your changes (Figma frames 27, 75, 97, 88, 89): the open pull and merge requests the reader wrote, per signed-in host, from
//! `mine.list`. The reading pane is [`crate::panel`]; the CI log is [`crate::checks`]. Rule 15: an empty list says "no open changes"
//! only when every source answered in full.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use chrono::Utc;
use craft::egui::{self, Context, Margin, Rect, ScrollArea, Ui};
use pr_craft_core::Forge;
use pr_craft_core::mine::Mine;
use pr_craft_engine::mine::{MineSource, MineState};
use pr_craft_forge::Login;

use crate::panel::Panel;
use crate::{PrCraftApp, age, components, keymap, live_label, select_hint, theme};

/// What a pending read of the saved state of a forge's own changes holds.
pub(crate) struct MineSync {
    pub(crate) forge: Forge,
    /// A read on its way: the list stays as it was until it answers.
    rx: Option<Receiver<MineSource>>,
    pub(crate) state: Option<MineState>,
    /// Why the last re-check failed while the list from the check before it is still shown.
    pub(crate) stale: Option<String>,
}

fn spawn(ctx: &Context, forge: Forge) -> Receiver<MineSource> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        // The window may have closed first; nobody is left to tell.
        let _ = tx.send(pr_craft_engine::mine::source(forge));
        ctx.request_repaint();
    });
    rx
}

impl MineSync {
    pub(crate) fn start(ctx: &Context, forge: Forge) -> Self {
        MineSync { forge, rx: Some(spawn(ctx, forge)), state: None, stale: None }
    }

    /// A source that has answered `changes`, for the render tests.
    #[cfg(test)]
    pub(crate) fn fixture(forge: Forge, changes: Vec<Mine>) -> Self {
        let listed = changes.len();
        let host = forge.default_host().to_string();
        let hosts = vec![pr_craft_engine::HostAnswer { host, listed, truncated: false, error: None }];
        MineSync { forge, rx: None, state: Some(MineState::Listed { changes, truncated: false, hosts, signed_out: Vec::new() }), stale: None }
    }

    /// Asks again because the reader asked or a write was made: the held reads are dropped and a poll on its way, which started
    /// before, is let go (its answer is never shown), so what arrives is a read made after.
    pub(crate) fn recheck(&mut self, ctx: &Context) {
        pr_craft_engine::mine::invalidate();
        self.rx = Some(spawn(ctx, self.forge));
    }

    /// Asks again, unless an answer is still on its way.
    pub(crate) fn restart(&mut self, ctx: &Context) {
        if self.rx.is_none() {
            self.rx = Some(spawn(ctx, self.forge));
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.rx.is_some()
    }

    /// Takes the answer when it has arrived. A re-check that fails keeps the last good list and says so.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let state = match rx.try_recv() {
            Ok(source) => source.state,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => MineState::Failed { error: "the sync stopped before answering".into() },
        };
        self.rx = None;
        if !matches!(state, MineState::Listed { .. }) && matches!(self.state, Some(MineState::Listed { .. })) {
            self.stale = Some(match state {
                MineState::Failed { error } => error,
                _ => format!("{} cannot act for you right now", self.forge.cli()),
            });
        } else {
            self.stale = None;
            self.state = Some(state);
        }
        true
    }
}

impl PrCraftApp {
    /// Every listed own change, newest update first.
    pub(crate) fn mine_items(&self) -> Vec<&Mine> {
        let mut items: Vec<&Mine> = self
            .mine
            .iter()
            .filter_map(|m| match &m.state {
                Some(MineState::Listed { changes, .. }) => Some(changes.iter()),
                _ => None,
            })
            .flatten()
            .collect();
        items.sort_by_key(|m| std::cmp::Reverse(m.change.updated_at));
        items
    }

    /// The own changes a reviewer waits on, as Back to you read them.
    pub(crate) fn yours_items(&self) -> Vec<&Mine> {
        let mut items: Vec<&Mine> = self
            .back
            .iter()
            .filter_map(|b| match &b.sync {
                crate::BackFetch::Done(pr_craft_engine::turn::BackState::Listed { yours, .. }) => Some(yours.iter()),
                _ => None,
            })
            .flatten()
            .collect();
        items.sort_by_key(|m| std::cmp::Reverse(m.change.updated_at));
        items
    }

    /// The selected own change, in Your changes or among those waiting in Back to you.
    pub(crate) fn selected_mine(&self) -> Option<&Mine> {
        let url = self.selected.as_deref()?;
        self.mine_items().into_iter().chain(self.yours_items()).find(|m| m.change.url == url)
    }

    pub(crate) fn mine_ids(&self) -> Vec<String> {
        self.mine_items().iter().map(|m| m.change.url.clone()).collect()
    }

    /// The sidebar count: `…` while a source answers for the first time, `+` when a change may be missing from it.
    pub(crate) fn mine_count(&self) -> String {
        if self.mine.iter().any(|m| m.state.is_none() && m.pending()) {
            return "…".into();
        }
        let missing = self.mine.iter().any(|m| match &m.state {
            Some(MineState::Listed { truncated, .. }) => *truncated || m.state.as_ref().is_some_and(MineState::incomplete),
            _ => true,
        });
        format!("{}{}", self.mine_items().len(), if missing { "+" } else { "" })
    }

    /// Takes the answers that have arrived. True when the list changed.
    pub(crate) fn poll_mine(&mut self) -> bool {
        self.mine.iter_mut().fold(false, |any, m| m.poll() | any)
    }

    /// Asks every forge's own changes again, now.
    pub(crate) fn restart_mine(&mut self, ctx: &Context) {
        for m in &mut self.mine {
            m.recheck(ctx);
        }
    }

    fn mine_notes(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        for m in &self.mine {
            let (name, cli) = (m.forge.name(), m.forge.cli());
            match &m.state {
                Some(MineState::Failed { error }) => live_label(ui, format!("{name}: could not read your changes: {error}"), p.rose_text),
                Some(MineState::Paused { login }) => {
                    let why = match login {
                        Login::LoggedOut { .. } => format!("{cli} is not logged in"),
                        Login::CliMissing { .. } => format!("{cli} is not installed"),
                        _ => format!("{cli} cannot act for you yet"),
                    };
                    live_label(ui, format!("{name}: {why}, so its changes are not listed. Sources on the left says how to fix it."), p.ember_text);
                }
                Some(MineState::Listed { hosts, truncated, .. }) => {
                    for host in hosts.iter().filter(|h| h.error.is_some()) {
                        live_label(ui, format!("{} could not be read: {}", host.host, host.error.as_deref().unwrap_or_default()), p.rose_text);
                    }
                    if *truncated {
                        live_label(
                            ui,
                            format!("{cli} returned its limit of {}; older changes may be missing.", pr_craft_forge::PAGE),
                            p.text_secondary,
                        );
                    }
                }
                None => {}
            }
            if let Some(why) = &m.stale {
                live_label(ui, format!("The last re-check of {cli} failed: {why}. This is the list from the check before it."), p.ember_text);
            }
        }
    }

    /// Whether some source left changes out, so an empty list is not an all-clear.
    fn mine_incomplete(&self) -> bool {
        self.mine.iter().any(|m| m.stale.is_some() || m.state.as_ref().is_none_or(MineState::incomplete))
    }

    /// Your changes: the middle pane.
    pub(crate) fn mine_list(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        let syncing = self.mine.iter().filter(|m| m.state.is_none() && m.pending()).count();
        let n = self.mine_items().len();
        let more = if self.mine_count().ends_with('+') { "+" } else { "" };
        let said = if syncing == 0 {
            format!("{n}{more} open")
        } else {
            format!("{n} so far. Syncing your sources, {} of {} answered.", self.mine.len() - syncing, self.mine.len())
        };
        components::padded(ui, Margin { left: 20, right: 20, top: 14, bottom: 12 }, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                components::label(ui, "Your changes", 16.0, p.graphite);
                let count = components::label(ui, said.clone(), 12.0, p.text_tertiary);
                ui.ctx().accesskit_node_builder(count.id, |node| node.set_live(egui::accesskit::Live::Polite));
            });
            components::note(ui, "Pull and merge requests you opened. Ember marks the ones where a reviewer is waiting on you.", p.text_secondary);
            self.mine_notes(ui);
        });
        components::rule(ui);
        if n == 0 && syncing == 0 {
            let (title, body) = if self.mine_incomplete() {
                (
                    "Nothing listed yet",
                    "A source is paused, failed or could not be re-checked, so an empty list is not an all-clear. The notes above say which.",
                )
            } else {
                ("You have no open changes", "Every source answered and none lists an open pull or merge request of yours.")
            };
            components::padded(ui, Margin::symmetric(20, 36), |ui| components::empty_state(ui, title, body, &[]));
            return;
        }
        let now = Utc::now();
        let items: Vec<Mine> = self.mine_items().into_iter().cloned().collect();
        let (mut clicked, mut opened) = (None, None);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for m in &items {
                let selected = self.selected.as_deref() == Some(m.change.url.as_str());
                let status = m.status();
                let meta = format!("{}  ·  you  ·  {}", m.change.reference(), age(now, m.change.updated_at));
                let model = components::MineRow {
                    forge: m.change.forge,
                    title: &m.change.title,
                    meta,
                    size: m.size(),
                    status: &status.text,
                    tone: status.tone,
                    waiting: m.waiting_on_you(),
                    selected,
                };
                let row = components::mine_row(ui, m.change.url.as_str(), &model);
                if row.clicked() {
                    clicked = Some(m.change.url.clone());
                }
                if row.has_focus() && keymap::pressed(ui, "inbox.open") {
                    opened = Some((m.change.change_ref(), m.change.title.clone()));
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

    /// The reading pane of Your changes.
    pub(crate) fn mine_detail(&mut self, ui: &mut Ui, pane: Rect) {
        let Some(mine) = self.selected_mine().cloned() else {
            components::empty_pane(ui, "Nothing selected", &select_hint(ui, "a change"));
            return;
        };
        self.show_panel(ui, pane, mine);
    }

    /// Draws the panel of `mine`, opening it when another change was shown before, and acts on what it asks for.
    pub(crate) fn show_panel(&mut self, ui: &mut Ui, pane: Rect, mine: Mine) {
        let ctx = ui.ctx().clone();
        if self.panel.as_ref().is_none_or(|panel| panel.key() != mine.change.url) {
            self.panel = Some(Panel::open(&ctx, &mine, self.live()));
        }
        let Some(panel) = &mut self.panel else { return };
        let wrote = panel.poll(&ctx);
        let out = panel.ui(ui, pane, &mine);
        if let Some(result) = wrote {
            let ok = result.is_ok();
            self.runs.status = Some(result);
            if ok || self.live() {
                // Whatever the forge says now is what shows next, also after a write that ran out of time.
                self.restart_mine(&ctx);
                self.refresh_back(&ctx, mine.change.forge);
            }
        }
        if out.review {
            self.open_review(&ctx, mine.change.change_ref(), mine.change.title.clone());
        }
        if out.checks {
            self.checks =
                Some(crate::checks::ChecksScreen::open(&ctx, mine.change.change_ref(), mine.change.title.clone(), out.check.as_deref(), self.live()));
        }
    }

    /// Whether this window reads the forges: a fixture app never does.
    pub(crate) fn live(&self) -> bool {
        self.drafts_live
    }

    /// Asks one forge's Back to you again, keeping the list shown until it answers.
    fn refresh_back(&mut self, ctx: &Context, forge: Forge) {
        let Some(back) = self.back.iter_mut().find(|b| b.forge == forge) else { return };
        // A poll already on its way started before the write: its receiver is dropped, so its answer is never shown.
        if let crate::BackFetch::Checking(rx) = crate::start_back(ctx, forge) {
            back.refresh = Some(rx);
        }
    }
}
