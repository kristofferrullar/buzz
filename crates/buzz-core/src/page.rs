//! NIP-PG page constants shared by the SDK (builders) and the relay (ingest).
//!
//! Kinds live in [`crate::kind`]; this module holds the tag names and limits so
//! both sides validate against one definition. See `docs/nips/NIP-PG.md`.

use crate::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION};

/// Tag naming the page a revision or suggestion belongs to (uuid v4).
pub const TAG_PAGE_ID: &str = "d";
/// Tag on a revision pointing at the revision it was based on.
pub const TAG_PREV: &str = "prev";
/// Tag carrying a revision's title.
pub const TAG_TITLE: &str = "title";
/// Tag on a revision that applies an accepted suggestion.
pub const TAG_SUGGESTION: &str = "suggestion";
/// Tag on a suggestion pointing at the revision it edits.
pub const TAG_BASE: &str = "base";
/// Tag on a resolution holding `accepted` or `rejected`.
pub const TAG_STATUS: &str = "status";
/// Tag on an accepted resolution pointing at the resulting revision.
pub const TAG_REV: &str = "rev";

/// Resolution status: the suggestion was applied.
pub const STATUS_ACCEPTED: &str = "accepted";
/// Resolution status: the suggestion was declined.
pub const STATUS_REJECTED: &str = "rejected";

/// Maximum page title length in bytes.
pub const MAX_PAGE_TITLE_BYTES: usize = 256;
/// Maximum page content length in bytes (matches the relay's `max_content_len`).
pub const MAX_PAGE_CONTENT_BYTES: usize = 64 * 1024;

/// Returns `true` if `kind` is one of the NIP-PG page kinds.
pub const fn is_page_kind(kind: u32) -> bool {
    kind == KIND_PAGE_REVISION
        || kind == KIND_PAGE_SUGGESTION
        || kind == KIND_PAGE_SUGGESTION_RESOLUTION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::{KIND_CANVAS, KIND_LONG_FORM};

    #[test]
    fn page_kinds_are_recognised() {
        assert!(is_page_kind(KIND_PAGE_REVISION));
        assert!(is_page_kind(KIND_PAGE_SUGGESTION));
        assert!(is_page_kind(KIND_PAGE_SUGGESTION_RESOLUTION));
    }

    #[test]
    fn neighbouring_kinds_are_not_page_kinds() {
        assert!(!is_page_kind(KIND_CANVAS));
        assert!(!is_page_kind(KIND_LONG_FORM));
        assert!(!is_page_kind(KIND_PAGE_REVISION - 1));
        assert!(!is_page_kind(KIND_PAGE_SUGGESTION_RESOLUTION + 1));
    }

    #[test]
    fn page_kinds_stay_inside_the_fork_private_block() {
        for k in [
            KIND_PAGE_REVISION,
            KIND_PAGE_SUGGESTION,
            KIND_PAGE_SUGGESTION_RESOLUTION,
        ] {
            assert!((52000..=52099).contains(&k), "kind {k} outside 52000–52099");
        }
    }
}
