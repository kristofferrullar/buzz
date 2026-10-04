//! NIP-PG page builders (kinds 52000–52002). See `docs/nips/NIP-PG.md`.

use buzz_core::{
    kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION},
    page::{
        MAX_PAGE_CONTENT_BYTES, MAX_PAGE_TITLE_BYTES, STATUS_ACCEPTED, STATUS_REJECTED, TAG_BASE,
        TAG_PAGE_ID, TAG_PREV, TAG_REV, TAG_STATUS, TAG_SUGGESTION, TAG_TITLE,
    },
};
use nostr::{EventBuilder, EventId, Kind, Tag};
use uuid::Uuid;

use crate::SdkError;

fn tag(parts: &[&str]) -> Result<Tag, SdkError> {
    Tag::parse(parts.iter().copied()).map_err(|e| SdkError::InvalidTag(e.to_string()))
}

fn check_content(content: &str) -> Result<(), SdkError> {
    let got = content.len();
    if got > MAX_PAGE_CONTENT_BYTES {
        return Err(SdkError::ContentTooLarge {
            max: MAX_PAGE_CONTENT_BYTES,
            got,
        });
    }
    Ok(())
}

fn check_title(title: &str) -> Result<(), SdkError> {
    if title.trim().is_empty() {
        return Err(SdkError::InvalidInput(
            "page title must not be empty".into(),
        ));
    }
    if title.len() > MAX_PAGE_TITLE_BYTES {
        return Err(SdkError::InvalidInput(format!(
            "page title exceeds {MAX_PAGE_TITLE_BYTES} bytes (got {})",
            title.len()
        )));
    }
    Ok(())
}

/// Build a page revision (kind 52000).
///
/// `prev` is the head revision this edit is based on; pass `None` only for the
/// first revision of a page. The relay rejects a stale `prev` with `conflict:`.
/// `suggestion` marks a revision that applies an accepted suggestion.
pub fn build_page_revision(
    channel_id: Uuid,
    page_id: Uuid,
    title: &str,
    content: &str,
    prev: Option<EventId>,
    suggestion: Option<EventId>,
) -> Result<EventBuilder, SdkError> {
    check_title(title)?;
    check_content(content)?;
    let mut tags = vec![
        tag(&["h", &channel_id.to_string()])?,
        tag(&[TAG_PAGE_ID, &page_id.to_string()])?,
        tag(&[TAG_TITLE, title])?,
    ];
    if let Some(prev) = prev {
        tags.push(tag(&[TAG_PREV, &prev.to_hex()])?);
    }
    if let Some(suggestion) = suggestion {
        tags.push(tag(&[TAG_SUGGESTION, &suggestion.to_hex()])?);
    }
    Ok(EventBuilder::new(Kind::Custom(KIND_PAGE_REVISION as u16), content).tags(tags))
}

/// Build a page suggestion (kind 52001): a proposed full-content edit of `base`.
pub fn build_page_suggestion(
    channel_id: Uuid,
    page_id: Uuid,
    base: EventId,
    content: &str,
) -> Result<EventBuilder, SdkError> {
    check_content(content)?;
    let tags = vec![
        tag(&["h", &channel_id.to_string()])?,
        tag(&[TAG_PAGE_ID, &page_id.to_string()])?,
        tag(&[TAG_BASE, &base.to_hex()])?,
    ];
    Ok(EventBuilder::new(Kind::Custom(KIND_PAGE_SUGGESTION as u16), content).tags(tags))
}

/// Outcome recorded by a suggestion resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageResolution {
    /// The suggestion was applied; `revision` is the revision the resolver published.
    Accepted {
        /// The resulting `PAGE_REVISION` event id.
        revision: EventId,
    },
    /// The suggestion was declined.
    Rejected,
}

/// Build a suggestion resolution (kind 52002).
pub fn build_page_suggestion_resolution(
    channel_id: Uuid,
    page_id: Uuid,
    suggestion: EventId,
    resolution: PageResolution,
) -> Result<EventBuilder, SdkError> {
    let mut tags = vec![
        tag(&["h", &channel_id.to_string()])?,
        tag(&[TAG_PAGE_ID, &page_id.to_string()])?,
        tag(&["e", &suggestion.to_hex()])?,
    ];
    match resolution {
        PageResolution::Accepted { revision } => {
            tags.push(tag(&[TAG_STATUS, STATUS_ACCEPTED])?);
            tags.push(tag(&[TAG_REV, &revision.to_hex()])?);
        }
        PageResolution::Rejected => tags.push(tag(&[TAG_STATUS, STATUS_REJECTED])?),
    }
    Ok(EventBuilder::new(Kind::Custom(KIND_PAGE_SUGGESTION_RESOLUTION as u16), "").tags(tags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Keys;

    fn sign(b: EventBuilder) -> nostr::Event {
        b.sign_with_keys(&Keys::generate()).unwrap()
    }

    fn tag_value(ev: &nostr::Event, name: &str) -> Option<String> {
        ev.tags
            .iter()
            .find(|t| t.as_slice().first().map(String::as_str) == Some(name))
            .and_then(|t| t.as_slice().get(1).cloned())
    }

    fn eid(byte: u8) -> EventId {
        EventId::from_byte_array([byte; 32])
    }

    #[test]
    fn first_revision_has_no_prev_tag() {
        let (cid, pid) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = sign(build_page_revision(cid, pid, "Plan", "# Plan", None, None).unwrap());
        assert_eq!(ev.kind.as_u16(), 52000);
        assert_eq!(tag_value(&ev, "h"), Some(cid.to_string()));
        assert_eq!(tag_value(&ev, "d"), Some(pid.to_string()));
        assert_eq!(tag_value(&ev, "title"), Some("Plan".into()));
        assert_eq!(tag_value(&ev, "prev"), None);
        assert_eq!(ev.content, "# Plan");
    }

    #[test]
    fn later_revision_carries_prev_and_suggestion() {
        let ev = sign(
            build_page_revision(
                Uuid::new_v4(),
                Uuid::new_v4(),
                "Plan",
                "v2",
                Some(eid(1)),
                Some(eid(2)),
            )
            .unwrap(),
        );
        assert_eq!(tag_value(&ev, "prev"), Some(eid(1).to_hex()));
        assert_eq!(tag_value(&ev, "suggestion"), Some(eid(2).to_hex()));
    }

    #[test]
    fn revision_rejects_blank_and_oversize_titles() {
        let (cid, pid) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(build_page_revision(cid, pid, "   ", "x", None, None).is_err());
        let long = "t".repeat(MAX_PAGE_TITLE_BYTES + 1);
        assert!(build_page_revision(cid, pid, &long, "x", None, None).is_err());
        let max = "t".repeat(MAX_PAGE_TITLE_BYTES);
        assert!(build_page_revision(cid, pid, &max, "x", None, None).is_ok());
    }

    #[test]
    fn revision_and_suggestion_bound_content() {
        let (cid, pid) = (Uuid::new_v4(), Uuid::new_v4());
        let big = "a".repeat(MAX_PAGE_CONTENT_BYTES + 1);
        assert!(matches!(
            build_page_revision(cid, pid, "T", &big, None, None),
            Err(SdkError::ContentTooLarge { .. })
        ));
        assert!(matches!(
            build_page_suggestion(cid, pid, eid(1), &big),
            Err(SdkError::ContentTooLarge { .. })
        ));
        let max = "a".repeat(MAX_PAGE_CONTENT_BYTES);
        assert!(build_page_revision(cid, pid, "T", &max, None, None).is_ok());
    }

    #[test]
    fn suggestion_carries_base_and_never_prev() {
        let (cid, pid) = (Uuid::new_v4(), Uuid::new_v4());
        let ev = sign(build_page_suggestion(cid, pid, eid(7), "proposed").unwrap());
        assert_eq!(ev.kind.as_u16(), 52001);
        assert_eq!(tag_value(&ev, "base"), Some(eid(7).to_hex()));
        assert_eq!(tag_value(&ev, "prev"), None);
        assert_eq!(tag_value(&ev, "h"), Some(cid.to_string()));
    }

    #[test]
    fn accepted_resolution_references_revision() {
        let ev = sign(
            build_page_suggestion_resolution(
                Uuid::new_v4(),
                Uuid::new_v4(),
                eid(3),
                PageResolution::Accepted { revision: eid(4) },
            )
            .unwrap(),
        );
        assert_eq!(ev.kind.as_u16(), 52002);
        assert_eq!(tag_value(&ev, "e"), Some(eid(3).to_hex()));
        assert_eq!(tag_value(&ev, "status"), Some("accepted".into()));
        assert_eq!(tag_value(&ev, "rev"), Some(eid(4).to_hex()));
    }

    #[test]
    fn rejected_resolution_has_no_rev_tag() {
        let ev = sign(
            build_page_suggestion_resolution(
                Uuid::new_v4(),
                Uuid::new_v4(),
                eid(3),
                PageResolution::Rejected,
            )
            .unwrap(),
        );
        assert_eq!(tag_value(&ev, "status"), Some("rejected".into()));
        assert_eq!(tag_value(&ev, "rev"), None);
    }
}
