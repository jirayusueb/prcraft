//! Keys a data file holds that this build does not know. Kept on load and written back on save, so a build never strips
//! what a newer or older build wrote. No I/O.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Used as `#[serde(flatten, default)] pub extra: Extra`. An empty one writes no keys.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extra(pub BTreeMap<String, Value>);

// JSON numbers from serde_json are never NaN, so equality is total.
impl Eq for Extra {}

impl Hash for Extra {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // The map keeps its keys sorted, so equal maps print the same text.
        serde_json::to_string(&self.0).unwrap_or_default().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unknown_keys_survive_and_an_empty_extra_writes_nothing() {
        #[derive(Debug, Default, Serialize, Deserialize)]
        struct File {
            #[serde(default)]
            known: u32,
            #[serde(flatten, default)]
            extra: Extra,
        }
        let file: File = serde_json::from_value(json!({"known": 1, "newer": {"a": [1, 2]}})).expect("loads");
        assert_eq!(serde_json::to_value(&file).expect("saves"), json!({"known": 1, "newer": {"a": [1, 2]}}));
        assert_eq!(serde_json::to_value(File::default()).expect("saves"), json!({"known": 0}));
    }

    /// `file` lacks every optional field; with a key this build does not know added, it must load, and save back with that key.
    fn keeps_what_it_does_not_know<T: Serialize + serde::de::DeserializeOwned>(file: Value) {
        let mut newer = file;
        newer["from_the_future"] = json!({"n": [1]});
        let loaded: T = serde_json::from_value(newer).expect("loads");
        assert_eq!(serde_json::to_value(&loaded).expect("saves")["from_the_future"], json!({"n": [1]}));
    }

    #[test]
    fn every_persisted_struct_keeps_unknown_keys_and_fills_missing_fields() {
        use crate::agent::{Agent, EngineSettings};
        use crate::diff_view::LayoutOptions;
        use crate::keys::Binding;
        use crate::review::LocalReview;
        keeps_what_it_does_not_know::<Binding>(json!({"key": "J"}));
        keeps_what_it_does_not_know::<LocalReview>(json!({}));
        keeps_what_it_does_not_know::<EngineSettings>(json!({}));
        keeps_what_it_does_not_know::<Agent>(json!({"id": "a", "name": "A", "prompt": "p", "engine": "claude_code"}));
        keeps_what_it_does_not_know::<LayoutOptions>(json!({}));
    }
}
