//! Review commands: open a change, mark files viewed, filter, write drafts, and post them as one review.

use std::path::Path;
use std::sync::Mutex;

use pr_craft_core::context::ExpandedLine;
use pr_craft_core::diff::{self, FileDiff};
use pr_craft_core::diff_view::LayoutOptions;
use pr_craft_core::review::{Anchor, Draft, LineRef, LocalReview, Verdict};
use pr_craft_core::{ChangeRef, Forge};
use pr_craft_forge::ForgeError;
use pr_craft_forge::review::{self as forge, ChangeMeta};
use serde::{Deserialize, Serialize};

use crate::{CommandError, store, view};

/// Serialises every load-change-save of a review file in this process, so a draft written while a
/// submit is posting is never overwritten by the submit's copy. Never held across a forge call.
static STORE: Mutex<()> = Mutex::new(());

/// `review.open`: everything the review screen shows.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewView {
    pub change: ChangeRef,
    pub meta: ChangeMeta,
    /// The commit this diff belongs to; `draft.add` takes it back.
    pub anchor: Anchor,
    /// The drafts were written on an earlier commit than this diff, so their line numbers may not
    /// match it. They still post against the commit they were written on.
    pub stale: bool,
    pub files: Vec<FileDiff>,
    pub local: LocalReview,
    /// This repository's saved file filter.
    pub filter: Vec<String>,
    /// This repository's saved diff options (split, whitespace, words, wrap).
    pub layout: LayoutOptions,
}

/// A line comment to add; `draft.add` gives it an id.
#[derive(Debug, Clone, Deserialize)]
pub struct NewDraft {
    pub path: String,
    #[serde(default)]
    pub old_path: Option<String>,
    #[serde(default)]
    pub start: Option<LineRef>,
    pub end: LineRef,
    pub body: String,
    /// [`ReviewView::anchor`] of the diff the line numbers come from.
    pub anchor: Anchor,
    /// Written on the rendered view of an SVG; the draft says so and still posts on `end`.
    #[serde(default)]
    pub on_rendered: bool,
}

/// `review.open`: fetches the change and its diff, both at once. Blocks on the CLI.
pub fn open(change: &ChangeRef) -> Result<ReviewView, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let (meta, text) = std::thread::scope(|scope| {
        let meta = scope.spawn(|| forge::change_meta(change));
        let text = forge::diff(change);
        (meta.join().unwrap_or_else(|_| Err("reading the change stopped".into())), text)
    });
    let meta = meta.map_err(CommandError::Forge)?;
    let files = diff::parse_unified(&text.map_err(CommandError::Forge)?);
    // A viewed file un-marks itself when its patch changed or it left the change.
    let local = edit(change, |local| {
        local.viewed.retain(|path, hash| files.iter().any(|file| &file.path == path && file.hash == *hash));
        Ok(())
    })?;
    let filter = store::load_filter(&dir, change.forge, change.host.as_deref(), &change.repo).map_err(CommandError::read)?;
    let layout = store::load_layout(&dir, change.forge, change.host.as_deref(), &change.repo).map_err(CommandError::read)?;
    let anchor = anchor_of(&meta);
    let stale = local.anchor.as_ref().is_some_and(|written| *written != anchor);
    Ok(ReviewView { change: change.clone(), meta, anchor, stale, files, local, filter, layout })
}

/// `review.set_layout`: saves the diff options of one repository (on `host`; `None` is the forge's default) and returns
/// them. The window is not known here, so a `split` the window is too narrow for is kept: the screen decides whether it fits.
pub fn set_layout(forge: Forge, host: Option<&str>, repo: &str, options: LayoutOptions) -> Result<LayoutOptions, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let _guard = lock()?;
    store::save_layout(&dir, forge, host, repo, options.clone()).map_err(CommandError::write)?;
    Ok(options)
}

/// The most lines one `review.expand` returns, so a huge file cannot fill the screen's memory.
pub const MAX_EXPAND: u32 = 20_000;

/// `review.expand`: lines of a file as the forge holds it, to fill a gap in the diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Expanded {
    pub path: String,
    pub side: view::Side,
    /// The lines asked for that the file has, numbered on `side`.
    pub lines: Vec<ExpandedLine>,
    /// How many lines the whole file has.
    pub total: u32,
}

/// `review.expand`: lines `from` to `to` (1-based, both included) of `path` on `side`. Reads the change's commits first;
/// a screen that holds them calls [`expand_at`]. Blocks on the CLI.
pub fn expand(change: &ChangeRef, path: &str, side: view::Side, from: u32, to: u32) -> Result<Expanded, CommandError> {
    let meta = forge::change_meta(change).map_err(CommandError::Forge)?;
    expand_at(change, &meta, path, side, from, to)
}

/// [`expand`] with the commits already read. The head side is the commit the diff shows.
pub fn expand_at(change: &ChangeRef, meta: &ChangeMeta, path: &str, side: view::Side, from: u32, to: u32) -> Result<Expanded, CommandError> {
    let bytes = view::blob_at(change, meta, path, side)?
        .ok_or_else(|| CommandError::Invalid(format!("{path} has no {} side, so there are no lines to show", side.word())))?;
    let text = String::from_utf8(bytes).map_err(|_| CommandError::Invalid(format!("{path} is not text, so it has no lines to show")))?;
    lines_of(path, side, &text, from, to)
}

/// Lines `from..=to` of `text`, clamped to the file; `total` counts all of it.
fn lines_of(path: &str, side: view::Side, text: &str, from: u32, to: u32) -> Result<Expanded, CommandError> {
    if from == 0 || to < from {
        return Err(CommandError::Invalid("lines are numbered from 1, and `to` may not be before `from`".into()));
    }
    let total = text.lines().count() as u32;
    let last = to.min(from.saturating_add(MAX_EXPAND - 1)).min(total);
    let take = (last + 1).saturating_sub(from) as usize;
    let lines =
        text.lines().zip(1..).skip(from as usize - 1).take(take).map(|(line, number)| ExpandedLine { number, text: line.to_string() }).collect();
    Ok(Expanded { path: path.to_string(), side, lines, total })
}

/// `review.local`: the review as it is on this Mac now.
pub fn local(change: &ChangeRef) -> Result<LocalReview, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let _guard = lock()?;
    store::load_review(&dir, change).map_err(CommandError::read)
}

pub use crate::store::Pending;

/// What `review.drafts_all` returns: every change with something of the reader's on this Mac, and the files that could not be read.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DraftsAll {
    pub changes: Vec<store::Pending>,
    pub unreadable: Vec<String>,
}

/// `review.drafts_all`: the changes that have local drafts, a pending verdict or comment, or a post that may have gone through,
/// newest edit first. Reads the `reviews/` folder only; no forge is asked.
pub fn drafts_all() -> Result<DraftsAll, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    // No lock: a review is saved by writing a temp file and renaming it over the old one, so a read sees the old file or the
    // new one, never half of one, and a long walk never holds back a save.
    let (changes, unreadable) = store::list_pending(&dir);
    Ok(DraftsAll { changes, unreadable })
}

/// `review.mark_viewed`: `hash` is the file's [`FileDiff::hash`]; `None` un-marks it.
pub fn mark_viewed(change: &ChangeRef, path: &str, hash: Option<u64>) -> Result<LocalReview, CommandError> {
    edit(change, |local| {
        match hash {
            Some(hash) => local.viewed.insert(path.to_string(), hash),
            None => local.viewed.remove(path),
        };
        Ok(())
    })
}

/// `review.filter`: saves include and exclude patterns for one repository (on `host`). An empty list clears them.
pub fn save_filter(forge: Forge, host: Option<&str>, repo: &str, patterns: Vec<String>) -> Result<(), CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let patterns = patterns.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    let _guard = lock()?;
    store::save_filter(&dir, forge, host, repo, patterns).map_err(CommandError::write)
}

/// `draft.add`. Every draft of one review belongs to one commit, so all of them post together.
pub fn draft_add(change: &ChangeRef, new: NewDraft) -> Result<LocalReview, CommandError> {
    draft_add_in(&store::data_dir().map_err(CommandError::read)?, change, new)
}

/// [`draft_add`] on the review files in `dir`; `draft.keep` takes this path so the anchor rules apply to AI drafts too.
pub(crate) fn draft_add_in(dir: &Path, change: &ChangeRef, new: NewDraft) -> Result<LocalReview, CommandError> {
    edit_in(dir, change, |local| {
        let body = non_empty(new.body)?;
        match &local.anchor {
            Some(written) if !local.drafts.is_empty() && *written != new.anchor => {
                return Err(CommandError::Invalid(format!(
                    "this change has new commits since your other drafts were written on {}. Submit or delete those drafts first, so each comment lands on the lines it was written for",
                    short(&written.head_sha)
                )));
            }
            _ => local.anchor = Some(new.anchor),
        }
        let id = local.next_id;
        local.next_id += 1;
        local.drafts.push(Draft { id, path: new.path, old_path: new.old_path, start: new.start, end: new.end, body, on_rendered: new.on_rendered });
        Ok(())
    })
}

/// `draft.edit`.
pub fn draft_edit(change: &ChangeRef, id: u64, body: String) -> Result<LocalReview, CommandError> {
    edit(change, |local| {
        let body = non_empty(body)?;
        let draft = local.drafts.iter_mut().find(|d| d.id == id).ok_or_else(|| CommandError::Invalid(format!("no draft {id}")))?;
        draft.body = body;
        Ok(())
    })
}

/// `draft.delete`. Deleting a draft that is already gone is not an error.
pub fn draft_delete(change: &ChangeRef, id: u64) -> Result<LocalReview, CommandError> {
    edit(change, |local| {
        forget_draft(local, id);
        Ok(())
    })
}

/// `review.compose`: the verdict and the general comment that post with the drafts.
pub fn compose(change: &ChangeRef, verdict: Verdict, body: String) -> Result<LocalReview, CommandError> {
    edit(change, |local| {
        if !Verdict::offered(change.forge).contains(&verdict) {
            return Err(CommandError::Invalid(format!("{} has no \"{}\" verdict", change.forge.name(), verdict.word())));
        }
        local.verdict = verdict;
        local.body = body;
        Ok(())
    })
}

/// `review.markdown`: the review as Markdown, for when the forge will not take it.
pub fn markdown(change: &ChangeRef, title: &str) -> Result<String, CommandError> {
    Ok(local(change)?.markdown(change, title))
}

/// `review.submit`: posts the drafts, the general comment and the verdict, then forgets what posted.
/// Each posted part is removed from the file on its own, so a failure or a retry never posts twice
/// and a draft written meanwhile stays.
///
/// A post that runs out of time may still have gone through, so it is neither forgotten nor retried: everything stays, the review is
/// flagged `post_unknown`, and the next submit refuses until it is called with `confirm_unknown` after the reader checked the forge.
///
/// Drafts written on an earlier commit whose lines the change's diff no longer has post only with `confirm_outdated`.
pub fn submit(change: &ChangeRef, confirm_unknown: bool, confirm_outdated: bool) -> Result<LocalReview, CommandError> {
    let sent = local(change)?;
    check_postable(change.forge, &sent)?;
    check_unknown_confirmed(change, &sent, confirm_unknown)?;
    let dir = store::data_dir().map_err(CommandError::read)?;
    let kept = |error: ForgeError| failed_post(&dir, change, error);
    let meta = forge::change_meta(change).map_err(kept)?;
    let moved = sent.anchor.as_ref().is_some_and(|written| *written != anchor_of(&meta));
    if moved && !confirm_outdated {
        let files = diff::parse_unified(&forge::diff(change).map_err(kept)?);
        check_outdated_confirmed(anchor_of(&meta).outdated_drafts(&sent, &files).len(), false)?;
    }
    let meta = anchored(meta, sent.anchor.as_ref());
    match change.forge {
        Forge::GitHub => {
            forge::github_submit(change, &meta, sent.verdict, &sent.body, &sent.drafts).map_err(kept)?;
            edit(change, |local| {
                for draft in &sent.drafts {
                    forget_draft(local, draft.id);
                }
                posted(local, &sent);
                Ok(())
            })
        }
        Forge::GitLab => {
            // GitLab takes one discussion per call.
            for draft in &sent.drafts {
                forge::gitlab_post_draft(change, &meta, draft).map_err(kept)?;
                edit(change, |local| {
                    forget_draft(local, draft.id);
                    Ok(())
                })?;
            }
            // Each step is forgotten as soon as it posts, so a retry after a failed approve sends no second note.
            if !sent.body.trim().is_empty() {
                forge::gitlab_post_note(change, &sent.body).map_err(kept)?;
                edit(change, |local| {
                    forget_body(local, &sent);
                    Ok(())
                })?;
            }
            if sent.verdict == Verdict::Approve {
                forge::gitlab_approve(change).map_err(kept)?;
            }
            edit(change, |local| {
                posted(local, &sent);
                Ok(())
            })
        }
    }
}

/// Refuses to post again while an earlier post's outcome is unknown, until the reader has said they checked the forge.
fn check_unknown_confirmed(change: &ChangeRef, sent: &LocalReview, confirm_unknown: bool) -> Result<(), CommandError> {
    if sent.post_unknown.is_none() || confirm_unknown {
        return Ok(());
    }
    Err(CommandError::Invalid(format!(
        "An earlier post to {forge} did not answer in time, and it may have gone through. Check {what} on {forge}, then submit again with confirm_unknown. Nothing was sent now.",
        forge = change.forge.name(),
        what = change.reference()
    )))
}

/// Refuses to post drafts that sit on changed lines until the reader has said yes to each of them.
fn check_outdated_confirmed(outdated: usize, confirm_outdated: bool) -> Result<(), CommandError> {
    if outdated == 0 || confirm_outdated {
        return Ok(());
    }
    Err(CommandError::Invalid(format!(
        "{outdated} {} on lines that changed since you wrote them. Submit again with confirm_outdated to post them anyway. Nothing was sent.",
        if outdated == 1 { "draft is" } else { "drafts are" }
    )))
}

/// What a failed post leaves: a post that ran out of time flags the review (and keeps all of it, to be posted only when the reader
/// says so); any other failure just keeps what did not post.
fn failed_post(dir: &Path, change: &ChangeRef, error: ForgeError) -> CommandError {
    if !pr_craft_forge::outcome_unknown(&error) {
        return CommandError::Forge(ForgeError::new(
            error.kind,
            format!("{}. What did not post is kept on this Mac; submitting again sends only that.", error.message),
        ));
    }
    let said = error.message.clone();
    let flagged = edit_in(dir, change, |local| {
        local.post_unknown = Some(said);
        Ok(())
    });
    let note = match flagged {
        Ok(_) => "Your comments are kept on this Mac, and the next Submit asks you to confirm first.".to_string(),
        Err(why) => format!("Your comments are kept on this Mac, but the review could not be marked: {why}"),
    };
    CommandError::Forge(ForgeError::new(error.kind, format!("{error} {note}")))
}

/// What a finished post clears: the parts that went, and the flag, because the post now has a known outcome.
fn posted(local: &mut LocalReview, sent: &LocalReview) {
    forget_summary(local, sent);
    local.post_unknown = None;
}

fn check_postable(forge: Forge, local: &LocalReview) -> Result<(), CommandError> {
    if !Verdict::offered(forge).contains(&local.verdict) {
        return Err(CommandError::Invalid(format!("{} has no \"{}\" verdict", forge.name(), local.verdict.word())));
    }
    if local.is_empty() {
        return Err(CommandError::Invalid("there is nothing to post: no comments, no summary and no verdict".into()));
    }
    if local.verdict == Verdict::RequestChanges && local.body.trim().is_empty() {
        return Err(CommandError::Invalid("Request changes needs a summary saying what to change".into()));
    }
    Ok(())
}

fn forget_draft(local: &mut LocalReview, id: u64) {
    local.drafts.retain(|d| d.id != id);
    if local.drafts.is_empty() {
        local.anchor = None;
    }
}

/// Clears the summary and verdict that posted, unless the reviewer changed them while it posted.
/// Viewed marks stay: they describe the reader's progress, not the posted review.
fn forget_summary(local: &mut LocalReview, sent: &LocalReview) {
    forget_body(local, sent);
    if local.verdict == sent.verdict {
        local.verdict = Verdict::Comment;
    }
}

fn forget_body(local: &mut LocalReview, sent: &LocalReview) {
    if local.body == sent.body {
        local.body.clear();
    }
}

fn anchor_of(meta: &ChangeMeta) -> Anchor {
    Anchor {
        head_sha: meta.diff_refs.as_ref().map_or_else(|| meta.head_sha.clone(), |refs| refs.head_sha.clone()),
        base_sha: meta.diff_refs.as_ref().map(|refs| refs.base_sha.clone()),
        start_sha: meta.diff_refs.as_ref().map(|refs| refs.start_sha.clone()),
    }
}

/// `meta` with its commits replaced by the ones the drafts were written on.
fn anchored(mut meta: ChangeMeta, anchor: Option<&Anchor>) -> ChangeMeta {
    let Some(anchor) = anchor else { return meta };
    meta.head_sha.clone_from(&anchor.head_sha);
    if let (Some(refs), Some(base), Some(start)) = (&mut meta.diff_refs, &anchor.base_sha, &anchor.start_sha) {
        refs.head_sha.clone_from(&anchor.head_sha);
        refs.base_sha.clone_from(base);
        refs.start_sha.clone_from(start);
    }
    meta
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn non_empty(body: String) -> Result<String, CommandError> {
    if body.trim().is_empty() { Err(CommandError::Invalid("a comment needs some text".into())) } else { Ok(body) }
}

pub(crate) fn lock() -> Result<std::sync::MutexGuard<'static, ()>, CommandError> {
    STORE.lock().map_err(|_| CommandError::write("an earlier save crashed; restart PR Craft"))
}

fn edit(change: &ChangeRef, apply: impl FnOnce(&mut LocalReview) -> Result<(), CommandError>) -> Result<LocalReview, CommandError> {
    edit_in(&store::data_dir().map_err(CommandError::read)?, change, apply)
}

fn edit_in(dir: &Path, change: &ChangeRef, apply: impl FnOnce(&mut LocalReview) -> Result<(), CommandError>) -> Result<LocalReview, CommandError> {
    let _guard = lock()?;
    let mut local = store::load_review(dir, change).map_err(CommandError::read)?;
    let before = local.clone();
    apply(&mut local)?;
    // With nothing left to post, an unknown outcome has nothing left to ask about.
    if local.is_empty() {
        local.post_unknown = None;
    }
    if local != before {
        store::save_review(dir, change, &local).map_err(CommandError::write)?;
    }
    Ok(local)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn only_a_review_with_something_in_it_posts() {
        let mut local = LocalReview::default();
        assert!(check_postable(Forge::GitHub, &local).is_err(), "an empty review");
        local.verdict = Verdict::Approve;
        assert!(check_postable(Forge::GitHub, &local).is_ok(), "a bare approval");
        local.verdict = Verdict::RequestChanges;
        assert!(check_postable(Forge::GitLab, &local).is_err(), "GitLab has no request changes");
        assert!(check_postable(Forge::GitHub, &local).is_err(), "request changes without a summary");
        local.body = "Lock first.".into();
        assert!(check_postable(Forge::GitHub, &local).is_ok());
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pr-craft-review-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).expect("a temp folder");
        dir
    }

    fn change() -> ChangeRef {
        ChangeRef { forge: Forge::GitHub, repo: "acme/api".into(), number: 7, host: None }
    }

    #[test]
    fn a_post_that_ran_out_of_time_keeps_everything_flags_the_review_and_the_next_submit_asks() {
        let (dir, change) = (temp_dir(), change());
        edit_in(&dir, &change, |local| {
            local.body = "Lock first.".into();
            local.verdict = Verdict::Approve;
            Ok(())
        })
        .expect("a review");
        let timeout = "gh did not answer in time. The post may already have gone through: check the PR before posting again.";
        let error = failed_post(&dir, &change, timeout.into()).to_string();
        assert!(error.contains("check the PR before posting again") && error.contains("asks you to confirm"), "{error}");
        let kept = store::load_review(&dir, &change).expect("saved");
        assert_eq!((kept.body.as_str(), kept.verdict), ("Lock first.", Verdict::Approve), "nothing was forgotten");
        assert!(kept.post_unknown.is_some_and(|why| why.contains("may already have gone through")));
        let refused = check_unknown_confirmed(&change, &kept_review(&dir, &change), false).expect_err("asks first");
        assert!(refused.to_string().contains("confirm_unknown"), "{refused}");
        assert!(check_unknown_confirmed(&change, &kept_review(&dir, &change), true).is_ok(), "after the reader checked");
        let mut now = kept_review(&dir, &change);
        let sent = now.clone();
        posted(&mut now, &sent);
        assert_eq!(now.post_unknown, None, "a post that went through has a known outcome");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outdated_drafts_are_refused_without_confirm_outdated_and_posted_with_it() {
        let refused = check_outdated_confirmed(2, false).expect_err("asks first").to_string();
        assert!(refused.contains("2 drafts are on lines that changed") && refused.contains("confirm_outdated"), "{refused}");
        assert!(check_outdated_confirmed(2, true).is_ok(), "after the reader said yes");
        assert!(check_outdated_confirmed(0, false).is_ok(), "nothing outdated");
    }

    fn kept_review(dir: &Path, change: &ChangeRef) -> LocalReview {
        store::load_review(dir, change).expect("saved")
    }

    #[test]
    fn a_failure_that_is_not_a_timeout_does_not_flag_the_review_and_an_emptied_review_loses_the_flag() {
        let (dir, change) = (temp_dir(), change());
        edit_in(&dir, &change, |local| {
            local.body = "Lock first.".into();
            Ok(())
        })
        .expect("a review");
        let error = failed_post(&dir, &change, "gh refused it: HTTP 422".into()).to_string();
        assert!(error.contains("kept on this Mac"), "{error}");
        assert_eq!(kept_review(&dir, &change).post_unknown, None, "a refusal wrote nothing, so nothing is in doubt");
        failed_post(&dir, &change, "gh did not answer in time. The post may already have gone through: check the PR before posting again.".into());
        edit_in(&dir, &change, |local| {
            local.body.clear();
            Ok(())
        })
        .expect("emptied");
        assert_eq!(kept_review(&dir, &change).post_unknown, None, "with nothing left to post there is nothing to ask about");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_summary_changed_while_posting_is_kept() {
        let sent = LocalReview { body: "old".into(), verdict: Verdict::Approve, ..LocalReview::default() };
        let mut now = LocalReview { body: "new".into(), verdict: Verdict::Approve, ..LocalReview::default() };
        forget_summary(&mut now, &sent);
        assert_eq!(now.body, "new", "text typed during the post stays");
        assert_eq!(now.verdict, Verdict::Comment, "the verdict that posted is cleared");
    }

    #[test]
    fn expand_returns_the_lines_asked_for_clamped_to_the_file_and_counts_all_of_it() {
        let text = "one\ntwo\r\nthree\nfour\n";
        let got = lines_of("a.txt", view::Side::Head, text, 2, 3).expect("in range");
        assert_eq!(got.lines.iter().map(|l| (l.number, l.text.as_str())).collect::<Vec<_>>(), [(2, "two"), (3, "three")]);
        assert_eq!(got.total, 4, "a final newline is not another line");
        let all = lines_of("a.txt", view::Side::Head, text, 3, u32::MAX).expect("to the end");
        assert_eq!(all.lines.len(), 2, "clamped at the end of the file");
        assert!(lines_of("a.txt", view::Side::Head, text, 9, 20).expect("past the end").lines.is_empty());
        assert!(lines_of("a.txt", view::Side::Head, text, 0, 3).is_err(), "lines start at 1");
        assert!(lines_of("a.txt", view::Side::Head, text, 3, 2).is_err());
    }

    #[test]
    fn drafts_post_against_the_commit_they_were_written_on() {
        let refs = pr_craft_forge::review::GitLabDiffRefs { base_sha: "b2".into(), start_sha: "s2".into(), head_sha: "h2".into() };
        let meta = ChangeMeta {
            title: String::new(),
            author: String::new(),
            url: String::new(),
            head_branch: String::new(),
            base_branch: String::new(),
            head_sha: "h2".into(),
            base_sha: None,
            diff_refs: Some(refs),
        };
        let written = Anchor { head_sha: "h1".into(), base_sha: Some("b1".into()), start_sha: Some("s1".into()) };
        let posted = anchored(meta, Some(&written));
        assert_eq!(anchor_of(&posted), written);
    }
}
