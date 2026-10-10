//! Event builders for NIP-PG page writes (kinds 52000-52002).
//!
//! Pure: parse the ids the webview sends, choose the right [`PageEdit`] or
//! [`PageResolution`], and delegate the tag shapes to `buzz-sdk` so a desktop
//! write is byte-for-byte what the CLI and the relay expect. Nothing here signs
//! or touches the network; the commands in the parent module do that.

use buzz_sdk_pkg::{
    build_page_revision, build_page_suggestion, build_page_suggestion_resolution, PageEdit,
    PageResolution,
};
use nostr::{EventBuilder, EventId};
use uuid::Uuid;

fn parse_uuid(label: &str, value: &str) -> Result<Uuid, String> {
    Uuid::parse_str(value).map_err(|_| format!("invalid {label} UUID: {value}"))
}

fn parse_event_id(label: &str, value: &str) -> Result<EventId, String> {
    EventId::from_hex(value).map_err(|error| format!("invalid {label} event ID: {error}"))
}

/// Build a `PAGE_REVISION`.
///
/// - no `prev`, no `suggestion`: the first revision of a new page;
/// - `prev` only: an edit of the head `prev` (the relay rejects a stale one with
///   `conflict:`);
/// - `prev` and `suggestion`: applies the suggestion, which is one event.
///
/// A `suggestion` without a `prev` is refused rather than guessed at, and an
/// empty `prev` is an invalid id, never "create".
pub(super) fn revision_builder(
    channel_id: &str,
    page_id: &str,
    title: &str,
    content: &str,
    prev: Option<&str>,
    suggestion: Option<&str>,
) -> Result<EventBuilder, String> {
    let edit = match (prev, suggestion) {
        (None, None) => PageEdit::Create,
        (Some(prev), None) => PageEdit::Edit {
            prev: parse_event_id("prev", prev)?,
        },
        (Some(prev), Some(suggestion)) => PageEdit::ApplySuggestion {
            prev: parse_event_id("prev", prev)?,
            suggestion: parse_event_id("suggestion", suggestion)?,
        },
        (None, Some(_)) => {
            return Err("a revision that applies a suggestion needs a prev".to_string())
        }
    };
    build_page_revision(
        parse_uuid("channel", channel_id)?,
        parse_uuid("page", page_id)?,
        title,
        content,
        edit,
    )
    .map_err(|error| format!("invalid page revision: {error}"))
}

/// Build a `PAGE_SUGGESTION`: a proposed full-content edit of the revision `base`.
pub(super) fn suggestion_builder(
    channel_id: &str,
    page_id: &str,
    base: &str,
    content: &str,
) -> Result<EventBuilder, String> {
    build_page_suggestion(
        parse_uuid("channel", channel_id)?,
        parse_uuid("page", page_id)?,
        parse_event_id("base", base)?,
        content,
    )
    .map_err(|error| format!("invalid page suggestion: {error}"))
}

/// Build a `PAGE_SUGGESTION_RESOLUTION` with status `rejected`. Acceptance is
/// not a resolution: it is the revision from [`revision_builder`] carrying the
/// `suggestion` tag, which closes the suggestion on its own.
pub(super) fn rejection_builder(
    channel_id: &str,
    page_id: &str,
    suggestion: &str,
) -> Result<EventBuilder, String> {
    build_page_suggestion_resolution(
        parse_uuid("channel", channel_id)?,
        parse_uuid("page", page_id)?,
        parse_event_id("suggestion", suggestion)?,
        PageResolution::Rejected,
    )
    .map_err(|error| format!("invalid page resolution: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;

    const CHANNEL: &str = "11111111-1111-4111-8111-111111111111";
    const PAGE: &str = "22222222-2222-4222-8222-222222222222";

    fn eid(byte: u8) -> String {
        EventId::from_byte_array([byte; 32]).to_hex()
    }

    fn sign(builder: EventBuilder) -> nostr::Event {
        builder
            .sign_with_keys(&Keys::generate())
            .expect("sign test event")
    }

    fn tag(event: &nostr::Event, name: &str) -> Option<String> {
        event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(String::as_str) == Some(name))
            .and_then(|t| t.as_slice().get(1).cloned())
    }

    #[test]
    fn creating_a_page_has_no_prev_or_suggestion() {
        let event = sign(revision_builder(CHANNEL, PAGE, "Plan", "# Plan", None, None).unwrap());
        assert_eq!(event.kind.as_u16(), 52000);
        assert_eq!(tag(&event, "h").as_deref(), Some(CHANNEL));
        assert_eq!(tag(&event, "d").as_deref(), Some(PAGE));
        assert_eq!(tag(&event, "title").as_deref(), Some("Plan"));
        assert_eq!(tag(&event, "prev"), None);
        assert_eq!(tag(&event, "suggestion"), None);
        assert_eq!(event.content, "# Plan");
    }

    #[test]
    fn editing_carries_the_head_it_was_based_on() {
        let prev = eid(1);
        let event = sign(revision_builder(CHANNEL, PAGE, "Plan", "v2", Some(&prev), None).unwrap());
        assert_eq!(tag(&event, "prev").as_deref(), Some(prev.as_str()));
        assert_eq!(tag(&event, "suggestion"), None);
    }

    #[test]
    fn accepting_is_one_revision_carrying_prev_and_suggestion() {
        let (prev, suggestion) = (eid(1), eid(2));
        let event = sign(
            revision_builder(CHANNEL, PAGE, "Plan", "v2", Some(&prev), Some(&suggestion)).unwrap(),
        );
        assert_eq!(event.kind.as_u16(), 52000);
        assert_eq!(tag(&event, "prev").as_deref(), Some(prev.as_str()));
        assert_eq!(
            tag(&event, "suggestion").as_deref(),
            Some(suggestion.as_str())
        );
    }

    #[test]
    fn a_suggestion_without_a_prev_is_refused_not_turned_into_a_create() {
        let suggestion = eid(2);
        assert!(revision_builder(CHANNEL, PAGE, "Plan", "v2", None, Some(&suggestion)).is_err());
    }

    #[test]
    fn an_empty_prev_is_an_invalid_id_not_a_create() {
        assert!(revision_builder(CHANNEL, PAGE, "Plan", "v2", Some(""), None).is_err());
    }

    #[test]
    fn bad_ids_are_rejected_before_anything_is_built() {
        assert!(revision_builder("not-a-uuid", PAGE, "Plan", "x", None, None).is_err());
        assert!(revision_builder(CHANNEL, "not-a-uuid", "Plan", "x", None, None).is_err());
        assert!(revision_builder(CHANNEL, PAGE, "Plan", "x", Some("zz"), None).is_err());
        assert!(suggestion_builder(CHANNEL, PAGE, "zz", "x").is_err());
        assert!(rejection_builder(CHANNEL, PAGE, "zz").is_err());
    }

    #[test]
    fn the_sdk_limits_still_apply() {
        assert!(revision_builder(CHANNEL, PAGE, "   ", "x", None, None).is_err());
        let long_title = "t".repeat(257);
        assert!(revision_builder(CHANNEL, PAGE, &long_title, "x", None, None).is_err());
        let big = "x".repeat(64 * 1024 + 1);
        assert!(revision_builder(CHANNEL, PAGE, "Plan", &big, None, None).is_err());
        assert!(suggestion_builder(CHANNEL, PAGE, &eid(1), &big).is_err());
    }

    #[test]
    fn a_suggestion_names_its_base() {
        let base = eid(1);
        let event = sign(suggestion_builder(CHANNEL, PAGE, &base, "proposed").unwrap());
        assert_eq!(event.kind.as_u16(), 52001);
        assert_eq!(tag(&event, "base").as_deref(), Some(base.as_str()));
        assert_eq!(event.content, "proposed");
    }

    #[test]
    fn a_rejection_is_a_rejected_resolution_with_no_resulting_revision() {
        let suggestion = eid(3);
        let event = sign(rejection_builder(CHANNEL, PAGE, &suggestion).unwrap());
        assert_eq!(event.kind.as_u16(), 52002);
        assert_eq!(tag(&event, "e").as_deref(), Some(suggestion.as_str()));
        assert_eq!(tag(&event, "status").as_deref(), Some("rejected"));
        assert_eq!(tag(&event, "rev"), None);
        assert!(event.content.is_empty());
    }
}
