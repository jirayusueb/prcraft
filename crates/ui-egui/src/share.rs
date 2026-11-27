//! The Share sheet (Figma frames 19, 43 and 63): pick a template and a size, watch the picture draw, then Copy it or save
//! it as a PNG. The model and the SVG come from the engine's `share` commands; the pixels are drawn on a worker thread
//! by `pr-craft-share-render`. Nothing here reads code: the sheet only holds the counts and titles the model carries.
//!
//! Not built, and said so on the sheet (AGENTS.md rule 15): rewriting the headline with the AI engine, and Save through
//! a macOS save dialog (the runtime hands apps no window to hang it on), so Save writes into `~/Downloads`.

use std::path::{Path, PathBuf};

use craft::egui::{self, Align, Align2, ColorImage, Context, Layout, Margin, RichText, ScrollArea, TextureHandle, TextureOptions, Ui};
use pr_craft_core::Utc;
use pr_craft_core::review::Verdict;
use pr_craft_core::share::{Include, ShareKind, ShareModel, ShareSize};
use pr_craft_core::stack::StackLayer;
use pr_craft_engine::review::ReviewView;
use pr_craft_engine::share::{self, Data, Look};
use pr_craft_share_render as render;

use crate::ai::Fetch;
use crate::components::{self, Pill, PillKind};
use crate::keymap;
use crate::live_label;
use crate::settings::{note, section};
use crate::theme;

pub(crate) enum Event {
    None,
    Close,
}

/// What the review screen knows of the change's stack: the lookup runs after `review.open`, and a failed lookup is not
/// "not stacked" (AGENTS.md rule 15).
pub(crate) enum Stacked<'a> {
    Reading,
    Failed(&'a str),
    Known(&'a [StackLayer]),
}

impl Stacked<'_> {
    fn layers(&self) -> &[StackLayer] {
        match self {
            Stacked::Known(layers) => layers,
            Stacked::Reading | Stacked::Failed(_) => &[],
        }
    }
}

/// What a picture is drawn from; the picture on screen is current only while this matches the sheet's choices and the
/// review's numbers. Save and Copy ship the picture, so it must never describe an earlier verdict or draft count.
#[derive(Debug, Clone, PartialEq)]
struct Draw {
    kind: ShareKind,
    size: ShareSize,
    include: Include,
    summary: String,
    /// Layers read so far, so the stack template redraws when the stack arrives.
    layers: usize,
    verdict: Verdict,
    drafts: usize,
    /// The general comment is written: with the verdict and drafts it decides whether a verdict counts as chosen.
    body_written: bool,
}

struct Picture {
    texture: TextureHandle,
    image: ColorImage,
    png: Vec<u8>,
    model: ShareModel,
    key: Draw,
}

pub(crate) struct ShareSheet {
    kind: ShareKind,
    size: ShareSize,
    include: Include,
    summary: String,
    /// The picture being drawn, with the model and key it is drawn from.
    drawing: Fetch<render::Rendered>,
    pending: Option<(ShareModel, Draw)>,
    /// The last key a draw was started for, so a failed draw is not retried every frame.
    attempted: Option<Draw>,
    picture: Option<Picture>,
    /// Why there is no picture.
    problem: Option<String>,
    /// What Save or Copy did.
    notice: Option<Result<String, String>>,
    /// Frame 43: the image could not be handed to the clipboard.
    copy_failed: Option<String>,
}

impl ShareSheet {
    pub(crate) fn open() -> Self {
        ShareSheet {
            kind: ShareKind::ReviewSummary,
            size: ShareSize::Wide1200x630,
            include: Include::default(),
            summary: String::new(),
            drawing: Fetch::Idle,
            pending: None,
            attempted: None,
            picture: None,
            problem: None,
            notice: None,
            copy_failed: None,
        }
    }

    fn key(&self, view: &ReviewView, stacked: &Stacked) -> Draw {
        Draw {
            kind: self.kind,
            size: self.size,
            include: self.include,
            summary: self.summary.clone(),
            layers: stacked.layers().len(),
            verdict: view.local.verdict,
            drafts: view.local.drafts.len(),
            body_written: !view.local.body.trim().is_empty(),
        }
    }

    /// Starts a draw when the choices moved on from the last one and no draw is running.
    fn refresh(&mut self, ctx: &Context, view: &ReviewView, stacked: &Stacked, key: &Draw) {
        let was_drawing = self.drawing.loading();
        self.drawing.poll();
        if was_drawing && !self.drawing.loading() {
            match std::mem::replace(&mut self.drawing, Fetch::Idle) {
                Fetch::Ready(drawn) => self.land(ctx, drawn),
                Fetch::Failed(error) => self.problem = Some(format!("Could not draw the image: {error}")),
                Fetch::Idle | Fetch::Loading(_) => {}
            }
        }
        if self.drawing.loading() || self.attempted.as_ref() == Some(key) {
            return;
        }
        self.attempted = Some(key.clone());
        let data = match self.kind {
            ShareKind::StackProgress => Data::Stack(&view.change, stacked.layers()),
            ShareKind::ReviewSummary | ShareKind::ReleaseNote => Data::Review(view),
        };
        let line = self.summary.trim();
        let look = Look { include: self.include, summary: (!line.is_empty()).then(|| line.to_string()) };
        match share::model(self.kind, &data, &look, Utc::now()) {
            Ok(model) => {
                let size = self.size;
                let svg = share::rendered(model.clone(), size).svg;
                self.pending = Some((model, key.clone()));
                self.problem = None;
                self.drawing = Fetch::start(ctx, move || render::render(&svg, size));
            }
            Err(error) => {
                self.picture = None;
                self.problem = Some(error.to_string());
            }
        }
    }

    fn land(&mut self, ctx: &Context, drawn: render::Rendered) {
        let Some((model, key)) = self.pending.take() else { return };
        let image = ColorImage::from_rgba_unmultiplied([drawn.width as usize, drawn.height as usize], &drawn.rgba);
        let texture = ctx.load_texture("share-preview", image.clone(), TextureOptions::LINEAR);
        self.picture = Some(Picture { texture, image, png: drawn.png, model, key });
    }

    /// The picture, when it is the one the choices on the sheet describe.
    fn current(&self, key: &Draw) -> Option<&Picture> {
        self.picture.as_ref().filter(|p| p.key == *key)
    }

    fn save(&mut self, key: &Draw, view: &ReviewView) {
        let Some(picture) = self.current(key) else {
            self.notice = Some(Err("The image is still being drawn. Save it when the preview has updated.".into()));
            return;
        };
        let result = save_in_downloads(&picture.png, self.kind, view.change.number, self.size);
        self.notice =
            Some(result.map(|path| format!("Saved {} ({} KB) in your Downloads folder.", path.display(), picture.png.len().div_ceil(1024))));
    }

    fn copy(&mut self, ctx: &Context, key: &Draw) {
        let Some(picture) = self.current(key) else {
            self.copy_failed = Some(
                "The image is not drawn yet, so there is nothing to put on the clipboard. Wait for the preview to update, or save it as a file."
                    .into(),
            );
            return;
        };
        ctx.copy_image(picture.image.clone());
        self.copy_failed = None;
        self.notice =
            Some(Ok("Sent the image to the clipboard. macOS does not tell PR Craft whether it took it; if it does not paste, use Save PNG.".into()));
    }

    /// ⌘S saves, ⌘C copies when no text field holds the selection, Esc leaves a field and then closes.
    fn keys(&mut self, ctx: &Context, key: &Draw, view: &ReviewView) -> Event {
        let typing = ctx.text_edit_focused();
        if keymap::consume(ctx, "share.save") {
            self.save(key, view);
        }
        // egui turns ⌘C into a Copy event, not a key press.
        if !typing && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
            self.copy(ctx, key);
        }
        if keymap::consume(ctx, "share.close") {
            if typing {
                if let Some(id) = ctx.memory(|m| m.focused()) {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            } else {
                return Event::Close;
            }
        }
        Event::None
    }

    pub(crate) fn ui(&mut self, ctx: &Context, view: &ReviewView, stacked: &Stacked) -> Event {
        let key = self.key(view, stacked);
        self.refresh(ctx, view, stacked, &key);
        let mut event = self.keys(ctx, &key, view);
        let p = theme::pal(ctx);
        let frame = egui::Frame::window(&ctx.global_style()).fill(p.raised).corner_radius(10).inner_margin(0);
        egui::Window::new("Share as image")
            .title_bar(false)
            .frame(frame)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, theme::SHEET_TOP])
            .min_width(theme::SHARE_CONTROLS_WIDTH + theme::SHARE_PREVIEW_WIDTH + 64.0)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                // In a short window the sheet scrolls rather than running off the bottom.
                let tallest = (ctx.content_rect().height() - theme::SHEET_TOP - 24.0).max(240.0);
                ScrollArea::vertical().max_height(tallest).min_scrolled_height(tallest).auto_shrink([true, true]).show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let side = egui::Frame::new().fill(p.hover).inner_margin(Margin::same(16)).corner_radius(egui::CornerRadius {
                            nw: 10,
                            sw: 10,
                            ne: 0,
                            se: 0,
                        });
                        let controls = side.show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_width(theme::SHARE_CONTROLS_WIDTH);
                                ui.spacing_mut().item_spacing.x = 8.0;
                                self.controls(ui, stacked);
                            });
                        });
                        components::seam_right(ui, controls.response.rect);
                        egui::Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.set_width(theme::SHARE_PREVIEW_WIDTH);
                                ui.spacing_mut().item_spacing.x = 8.0;
                                self.preview(ui, &key, view, &mut event);
                            });
                        });
                    });
                });
            });
        self.refresh(ctx, view, stacked, &self.key(view, stacked));
        event
    }

    fn controls(&mut self, ui: &mut Ui, stacked: &Stacked) {
        let p = theme::pal(ui);
        components::label(ui, "Share as image", 15.0, p.graphite);
        note(ui, "Titles, numbers and people. Never code, never comments.");
        section(ui, "Template");
        for kind in ShareKind::ALL {
            let available = kind != ShareKind::StackProgress || stacked.layers().len() >= 2;
            if available {
                if components::verdict_card(ui, self.kind == kind, kind.word(), kind.hint()).clicked() {
                    self.kind = kind;
                }
            } else if kind == ShareKind::StackProgress {
                components::label(ui, kind.word(), 13.0, p.text_tertiary);
                match stacked {
                    Stacked::Reading => live_label(ui, "Checking whether this change is stacked…", p.text_secondary),
                    Stacked::Failed(error) => {
                        live_label(ui, format!("Could not check whether this change is stacked: {error}"), p.rose_text);
                    }
                    Stacked::Known(_) => note(ui, "This change is not in a stack."),
                }
            }
        }
        section(ui, "Size");
        let sizes: Vec<(ShareSize, String)> = ShareSize::ALL.iter().map(|size| (*size, size.word().to_string())).collect();
        if let Some(size) = components::choose(ui, "Size", &sizes, self.size) {
            self.size = size;
        }
        components::note(ui, self.size.dimensions().to_string(), p.text_tertiary);
        section(ui, "Include");
        for (on, label) in
            [(&mut self.include.diff_stats, "Diff stats"), (&mut self.include.people, "People"), (&mut self.include.repo_name, "Repository name")]
        {
            if components::check_row(ui, *on, label, "", "").clicked() {
                *on = !*on;
            }
        }
        components::note(ui, "Repository name is off until you turn it on: share it only where the repository is not private.", p.text_tertiary);
        components::note(
            ui,
            "Diff stats off also replaces the big number: draft comments in a review summary, \"1 change\" in a release note.",
            p.text_tertiary,
        );
        section(ui, "Summary line");
        let stack = self.kind == ShareKind::StackProgress;
        ui.add_enabled_ui(!stack, |ui| {
            let label = components::label(ui, "Your words", 12.0, p.text_secondary);
            let field = egui::TextEdit::singleline(&mut self.summary)
                .hint_text("One line under the headline")
                .char_limit(400)
                .desired_width(ui.available_width());
            ui.add(field).labelled_by(label.id);
        });
        if stack {
            note(ui, "Stack progress has no room for a summary line.");
        }
        components::note(
            ui,
            "Rewriting the headline with the AI engine is not built; write the line yourself. Checks and approvals are not read yet, so no template shows them.",
            p.text_tertiary,
        );
    }

    fn preview(&mut self, ui: &mut Ui, key: &Draw, view: &ReviewView, event: &mut Event) {
        let p = theme::pal(ui);
        let ready = self.current(key).is_some();
        let facts = self.picture.as_ref().map(|picture| {
            let [w, h] = picture.image.size;
            format!("{w} × {h} · PNG · {} KB", picture.png.len().div_ceil(1024))
        });
        ui.horizontal(|ui| {
            components::label(ui, "Preview", 13.0, p.graphite);
            components::mono_label(ui, facts.clone().unwrap_or_default(), 11.5, p.text_tertiary);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if Pill::new(PillKind::Primary, "Copy image").key_text("⌘C").small().enabled(ready).show(ui).clicked() {
                    self.copy(ui.ctx(), key);
                }
                if Pill::new(PillKind::Secondary, "Save PNG").key(ui, "share.save").small().enabled(ready).show(ui).clicked() {
                    self.save(key, view);
                }
            });
        });
        if let Some(problem) = &self.problem {
            live_label(ui, problem.as_str(), p.rose_text);
        } else if self.picture.is_none() {
            live_label(ui, "Drawing the image…", p.text_secondary);
        }
        if let Some(picture) = &self.picture {
            let [w, h] = picture.image.size;
            let scale = (theme::SHARE_PREVIEW_WIDTH / w as f32).min(theme::SHARE_PREVIEW_HEIGHT / h as f32).min(1.0);
            let alt = picture.model.alt();
            ui.add_space(theme::SPACE_2);
            ui.add(
                egui::Image::new(egui::load::SizedTexture::new(picture.texture.id(), egui::vec2(w as f32 * scale, h as f32 * scale))).alt_text(alt),
            );
            components::note(ui, format!("Shown at {}%. Saves at full size.", (scale * 100.0).round()), p.text_tertiary);
            if picture.key != *key && self.problem.is_none() {
                live_label(ui, "Updating the preview…", p.text_secondary);
            }
            for text in &picture.model.left_out {
                ui.label(RichText::new(text).size(theme::sz(ui).body).color(p.ember_text));
            }
        }
        ui.add_space(theme::SPACE_2);
        self.copy_failed_card(ui);
        if let Some(notice) = &self.notice {
            match notice {
                Ok(text) => live_label(ui, text.as_str(), p.text_secondary),
                Err(text) => live_label(ui, text.as_str(), p.rose_text),
            }
        }
        ui.horizontal(|ui| {
            if Pill::new(PillKind::Quiet, "Close").key(ui, "share.close").small().show(ui).clicked() {
                *event = Event::Close;
            }
        });
        let (save, close) = (keymap::label(ui, "share.save"), keymap::label(ui, "share.close"));
        components::note(ui, format!("{save} save · ⌘C copy · {close} close · Tab moves between the controls"), p.text_tertiary);
    }

    /// Figma frame 43: the copy did not go through; say why and offer what works.
    fn copy_failed_card(&mut self, ui: &mut Ui) {
        let Some(reason) = self.copy_failed.clone() else { return };
        crate::review::card(ui, |ui| {
            live_label(ui, "Could not copy the image", theme::pal(ui).rose_text);
            note(ui, &reason);
            ui.horizontal(|ui| {
                if self.size != ShareSize::Slack && Pill::new(PillKind::Secondary, "Use Slack size").small().show(ui).clicked() {
                    self.size = ShareSize::Slack;
                    self.copy_failed = None;
                }
                if Pill::new(PillKind::Quiet, "Dismiss").small().show(ui).clicked() {
                    self.copy_failed = None;
                }
            });
        });
    }
}

/// `~/Downloads/pr-craft-….png`: saved under the first name no file has. An earlier image is never replaced.
fn save_in_downloads(png: &[u8], kind: ShareKind, number: u64, size: ShareSize) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set, so there is no Downloads folder to save into")?;
    save_new_in(png, &PathBuf::from(home).join("Downloads"), &format!("pr-craft-{}-{number}-{}", kind.slug(), size.slug()))
}

/// Writes `dir/stem.png`, or `dir/stem-2.png` and on. Each name is created exclusively and a taken name is skipped, so
/// a file or link that appears at any moment is never overwritten or followed.
fn save_new_in(png: &[u8], dir: &Path, stem: &str) -> Result<PathBuf, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder, so the image has nowhere to go", dir.display()));
    }
    for n in 1..1000 {
        let name = if n == 1 { format!("{stem}.png") } else { format!("{stem}-{n}.png") };
        let path = dir.join(name);
        match render::write_new(png, &path) {
            Ok(_) => return Ok(path),
            Err(render::NewError::Exists) => {}
            Err(render::NewError::Failed(error)) => return Err(error),
        }
    }
    Err(format!("{} already holds a thousand copies of this image", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_never_replaces_an_image_already_in_the_folder() {
        let dir = std::env::temp_dir().join(format!("pr-craft-share-test-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        assert_eq!(save_new_in(b"earlier", &dir, "image").ok(), Some(dir.join("image.png")));
        let second = save_new_in(b"later", &dir, "image").unwrap_or_default();
        assert_ne!(second, dir.join("image.png"));
        assert_eq!(std::fs::read(dir.join("image.png")).ok().as_deref(), Some(b"earlier".as_slice()));
        assert!(save_new_in(b"x", &dir.join("missing"), "image").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copying_before_the_picture_is_drawn_says_so_instead_of_sending_nothing() {
        let ctx = Context::default();
        let mut sheet = ShareSheet::open();
        let view = test_view();
        let key = sheet.key(&view, &Stacked::Reading);
        sheet.copy(&ctx, &key);
        assert!(sheet.copy_failed.is_some());
        assert!(sheet.notice.is_none(), "a copy that did not go out must not report success");
    }

    fn test_view() -> ReviewView {
        use pr_craft_core::review::{Anchor, LocalReview};
        use pr_craft_core::{ChangeRef, Forge};
        use pr_craft_forge::review::ChangeMeta;
        let meta = ChangeMeta {
            title: "t".into(),
            author: "a".into(),
            url: String::new(),
            head_branch: "h".into(),
            base_branch: "b".into(),
            head_sha: "abc1234".into(),
            base_sha: None,
            diff_refs: None,
        };
        ReviewView {
            change: ChangeRef { forge: Forge::GitHub, repo: "a/b".into(), number: 1, host: None },
            meta,
            anchor: Anchor { head_sha: "abc1234".into(), base_sha: None, start_sha: None },
            stale: false,
            files: Vec::new(),
            local: LocalReview::default(),
            filter: Vec::new(),
            layout: Default::default(),
        }
    }

    /// The numbers on the image come from the verdict and the drafts, so the picture is stale once either moves.
    #[test]
    fn the_verdict_and_the_draft_count_are_part_of_what_a_picture_is_drawn_from() {
        use pr_craft_core::review::{Draft, LineRef, Verdict};
        let sheet = ShareSheet::open();
        let mut view = test_view();
        let before = sheet.key(&view, &Stacked::Reading);
        view.local.verdict = Verdict::Approve;
        let approved = sheet.key(&view, &Stacked::Reading);
        assert_ne!(before, approved);
        view.local.body = "ok".into();
        let with_body = sheet.key(&view, &Stacked::Reading);
        assert_ne!(approved, with_body);
        let draft = Draft { id: 1, path: "a.rs".into(), old_path: None, start: None, end: LineRef { old: None, new: Some(1) }, body: "x".into() };
        view.local.drafts.push(draft);
        assert_ne!(with_body, sheet.key(&view, &Stacked::Reading));
    }
}
