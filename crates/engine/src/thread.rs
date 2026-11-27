//! `thread.list`, `thread.reply` and `thread.resolve`: the review threads on the forge.

use pr_craft_core::thread::ReviewThread;
use pr_craft_core::{ChangeRef, Forge};
use pr_craft_forge::review::change_meta;
use pr_craft_forge::threads as forge;
use serde::Serialize;

use crate::CommandError;

/// `thread.list`: every review thread of a change as the forge holds it.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadList {
    pub change: ChangeRef,
    pub threads: Vec<ReviewThread>,
    /// Every thread and every comment was read.
    pub complete: bool,
    /// What was not read, when `complete` is false.
    pub note: Option<String>,
}

/// The threads of `change`. Blocks on the CLI.
pub fn list(change: &ChangeRef) -> Result<ThreadList, CommandError> {
    // Only GitLab judges a thread outdated by the head commit; GitHub says so itself, so it needs no read of the change.
    let head = match change.forge {
        Forge::GitHub => String::new(),
        Forge::GitLab => {
            let meta = change_meta(change).map_err(CommandError::Forge)?;
            meta.diff_refs.map_or(meta.head_sha, |refs| refs.head_sha)
        }
    };
    let read = forge::list(change, &head).map_err(CommandError::Forge)?;
    Ok(ThreadList { change: change.clone(), threads: read.threads, complete: read.complete, note: read.note })
}

/// `thread.reply`: posts `body` to the thread on the forge now. An empty body is refused.
pub fn reply(change: &ChangeRef, thread: &str, body: String) -> Result<(), CommandError> {
    if body.trim().is_empty() {
        return Err(CommandError::Invalid("A reply needs some text.".into()));
    }
    let request = forge::reply_request(change, thread, &body).map_err(CommandError::Invalid)?;
    forge::execute(&request).map_err(CommandError::Forge)
}

/// `thread.resolve`: resolves the thread on the forge now, or reopens it with `resolved` false.
pub fn resolve(change: &ChangeRef, thread: &str, resolved: bool) -> Result<(), CommandError> {
    let request = forge::resolve_request(change, thread, resolved).map_err(CommandError::Invalid)?;
    forge::execute(&request).map_err(CommandError::Forge)
}
