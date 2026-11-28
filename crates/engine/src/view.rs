//! File view commands: read a file on either side of a change, and remember how each file type is shown.

use std::collections::BTreeMap;

use pr_craft_core::ChangeRef;
use pr_craft_core::view::{self, FileView};
use pr_craft_forge::blob as forge_blob;
use pr_craft_forge::review::{self as forge, ChangeMeta};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CommandError, review, store};

/// Which version of a file: the one the change starts from or the one it ends at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Base,
    Head,
}

impl Side {
    pub fn word(self) -> &'static str {
        match self {
            Side::Base => "base",
            Side::Head => "head",
        }
    }
}

/// `review.blob` for the library: the bytes of `path` on `side`, `None` when that side has no such file.
/// Reads the change's commits first; a screen that holds them calls [`blob_at`]. Blocks on the CLI.
pub fn blob(change: &ChangeRef, path: &str, side: Side) -> Result<Option<Vec<u8>>, CommandError> {
    let meta = forge::change_meta(change).map_err(CommandError::Forge)?;
    blob_at(change, &meta, path, side)
}

/// [`blob`] with the commits already read. The head is the commit the diff shows, so what a rendered view draws is what
/// the source view's lines belong to.
pub fn blob_at(change: &ChangeRef, meta: &ChangeMeta, path: &str, side: Side) -> Result<Option<Vec<u8>>, CommandError> {
    let sha = match side {
        Side::Head => meta.diff_refs.as_ref().map_or(meta.head_sha.as_str(), |refs| refs.head_sha.as_str()),
        Side::Base => meta.base_sha.as_deref().ok_or_else(|| {
            CommandError::Forge("the forge did not name the commit this change started from, so the old file cannot be read".into())
        })?,
    };
    forge_blob::blob(change, sha, path).map_err(CommandError::Forge)
}

/// `review.blob`'s result: whether the side has the file, and its bytes as base64.
pub fn blob_json(bytes: Option<Vec<u8>>) -> Value {
    match bytes {
        Some(bytes) => json!({ "present": true, "size": bytes.len(), "base64": base64(&bytes) }),
        None => json!({ "present": false }),
    }
}

// shortcut: L5 cannot use the craft facade; move to a std-only craft-libs codec when one exists.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |acc, (i, byte)| acc | (u32::from(*byte) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((group >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `review.views`: the view chosen for each file extension.
pub fn views() -> Result<BTreeMap<String, FileView>, CommandError> {
    let dir = store::data_dir().map_err(CommandError::read)?;
    let _guard = review::lock()?;
    store::load_views(&dir).map_err(CommandError::read)
}

/// `review.set_view`: remembers `view` for every file with `extension`, and returns all choices. A view the extension
/// does not offer is refused, so a stored choice always applies.
pub fn set_view(extension: &str, view: FileView) -> Result<BTreeMap<String, FileView>, CommandError> {
    let ext = extension.trim().trim_start_matches('.').to_ascii_lowercase();
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(CommandError::Invalid("an extension is letters and digits, like `md`".into()));
    }
    if !view::views_for(&format!("x.{ext}")).contains(&view) {
        return Err(CommandError::Invalid(format!(".{ext} files have no {} view", view.label())));
    }
    let dir = store::data_dir().map_err(CommandError::read)?;
    let _guard = review::lock()?;
    let mut all = store::load_views(&dir).map_err(CommandError::read)?;
    all.insert(ext, view);
    store::save_views(&dir, &all).map_err(CommandError::write)?;
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (plain, encoded) in
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain}");
        }
        assert_eq!(base64(&[0xFB, 0xFF, 0xFE]), "+//+", "the last two alphabet characters");
    }

    #[test]
    fn a_missing_side_is_not_present_rather_than_an_error() {
        assert_eq!(blob_json(None)["present"], false);
        let present = blob_json(Some(b"hi".to_vec()));
        assert_eq!((present["present"].as_bool(), present["size"].as_u64(), present["base64"].as_str()), (Some(true), Some(2), Some("aGk=")));
    }

    #[test]
    fn a_view_the_extension_does_not_offer_or_a_bad_extension_is_refused() {
        assert!(set_view("md", FileView::ImageSwipe).is_err());
        assert!(set_view("rs", FileView::Rendered).is_err());
        assert!(set_view("", FileView::Source).is_err());
        assert!(set_view("a/b", FileView::Source).is_err());
    }
}
