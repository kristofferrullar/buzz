//! Unit tests for the stateless page rules and the reference check. The rules
//! that need stored pages (conflicts, closure, atomicity) are covered end to end
//! against a real relay in `crates/buzz-test-client/tests/e2e_pages.rs`.

use buzz_core::kind::{KIND_CANVAS, KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION};
use buzz_sdk::pages::{
    build_page_revision, build_page_suggestion, build_page_suggestion_resolution, PageEdit,
    PageResolution,
};
use nostr::{EventBuilder, EventId, Keys, Kind, Tag};

use super::*;

fn sign(builder: EventBuilder) -> Event {
    builder
        .sign_with_keys(&Keys::generate())
        .expect("sign test event")
}

fn raw(kind: u32, tags: &[&[&str]], content: &str) -> Event {
    sign(
        EventBuilder::new(Kind::Custom(kind as u16), content).tags(
            tags.iter()
                .map(|tag| Tag::parse(tag.iter().copied()).expect("tag")),
        ),
    )
}

fn id(byte: u8) -> EventId {
    EventId::from_byte_array([byte; 32])
}

fn expect_err(event: &Event) -> String {
    parse_page_event(event).expect_err("event must be rejected")
}

#[test]
fn sdk_built_events_parse_for_all_three_kinds() {
    let (channel, page) = (Uuid::new_v4(), Uuid::new_v4());
    let create =
        sign(build_page_revision(channel, page, "Plan", "# Plan", PageEdit::Create).unwrap());
    let parsed = parse_page_event(&create).expect("create parses");
    assert_eq!((parsed.channel_id, parsed.page_id), (channel, page));
    assert!(matches!(
        parsed.body,
        PageBody::Revision {
            suggestion: None,
            ..
        }
    ));

    let apply = sign(
        build_page_revision(
            channel,
            page,
            "Plan",
            "# Plan 2",
            PageEdit::ApplySuggestion {
                prev: id(1),
                suggestion: id(2),
            },
        )
        .unwrap(),
    );
    let PageBody::Revision { meta, suggestion } = parse_page_event(&apply).unwrap().body else {
        panic!("expected a revision");
    };
    assert_eq!(meta.prev, Some([1; 32]));
    assert_eq!(suggestion, Some([2; 32]));

    let suggest = sign(build_page_suggestion(channel, page, id(1), "# Other").unwrap());
    assert_eq!(
        parse_page_event(&suggest).unwrap().body,
        PageBody::Suggestion { base: [1; 32] }
    );

    for (resolution, expected) in [
        (
            PageResolution::Accepted { revision: id(3) },
            Outcome::Accepted { rev: [3; 32] },
        ),
        (PageResolution::Rejected, Outcome::Rejected),
    ] {
        let event =
            sign(build_page_suggestion_resolution(channel, page, id(2), resolution).unwrap());
        assert_eq!(
            parse_page_event(&event).unwrap().body,
            PageBody::Resolution {
                suggestion: [2; 32],
                outcome: expected
            }
        );
    }
}

#[test]
fn non_page_kinds_are_not_parsed_as_pages() {
    let event = raw(KIND_CANVAS, &[], "");
    assert!(expect_err(&event).starts_with("invalid:"));
}

#[test]
fn tag_rules_reject_with_stable_invalid_messages() {
    let channel = Uuid::new_v4().to_string();
    let page = Uuid::new_v4().to_string();
    let hex = "a".repeat(64);
    let h = ["h", channel.as_str()];
    let d = ["d", page.as_str()];
    let title = ["title", "Plan"];

    // (event, substring of the stable message)
    let cases: Vec<(Event, &str)> = vec![
        (raw(52000, &[&d, &title], ""), "exactly one h tag"),
        (raw(52000, &[&h, &title], ""), "exactly one d tag"),
        (raw(52000, &[&h, &h, &d, &title], ""), "exactly one h tag"),
        (
            raw(
                52000,
                &[&h, &d, &["d", &Uuid::new_v4().to_string()], &title],
                "",
            ),
            "exactly one d tag",
        ),
        (raw(52000, &[&h, &d], ""), "exactly one title tag"),
        (
            raw(52000, &[&h, &d, &["title", "  "]], ""),
            "title must not be blank",
        ),
        (
            raw(52000, &[&h, &d, &["title", &"é".repeat(129)]], ""),
            "title exceeds 256 bytes",
        ),
        (
            raw(52000, &[&["h", &channel.to_uppercase()], &d, &title], ""),
            "canonical lowercase UUID",
        ),
        (
            raw(52000, &[&h, &["d", &page.to_uppercase()], &title], ""),
            "canonical lowercase UUID",
        ),
        (
            raw(52000, &[&h, &d, &title, &["prev", &hex.to_uppercase()]], ""),
            "64 lowercase hex",
        ),
        (
            raw(
                52000,
                &[&h, &d, &title, &["prev", &hex], &["prev", &hex]],
                "",
            ),
            "at most one prev",
        ),
        (
            raw(52000, &[&h, &d, &title, &["suggestion", &hex]], ""),
            "must carry prev",
        ),
        (
            raw(52000, &[&h, &d, &title, &["e", &hex]], ""),
            "must not carry an e tag",
        ),
        (
            raw(52000, &[&h, &d, &["title", "Plan", "extra"]], ""),
            "title tag must have exactly one value",
        ),
        (raw(52001, &[&h, &d], "x"), "exactly one base tag"),
        (
            raw(52001, &[&h, &d, &["base", &hex], &["e", &hex]], "x"),
            "must not carry an e tag",
        ),
        (
            raw(52002, &[&h, &d, &["status", "rejected"]], ""),
            "exactly one e tag",
        ),
        (
            raw(52002, &[&h, &d, &["e", &hex]], ""),
            "exactly one status tag",
        ),
        (
            raw(52002, &[&h, &d, &["e", &hex], &["status", "maybe"]], ""),
            "status must be accepted or rejected",
        ),
        (
            raw(52002, &[&h, &d, &["e", &hex], &["status", "accepted"]], ""),
            "accepted resolution must carry a rev",
        ),
        (
            raw(
                52002,
                &[
                    &h,
                    &d,
                    &["e", &hex],
                    &["status", "rejected"],
                    &["rev", &hex],
                ],
                "",
            ),
            "rejected resolution must not carry a rev",
        ),
    ];
    for (event, expected) in cases {
        let message = expect_err(&event);
        assert!(
            message.starts_with("invalid:") && message.contains(expected),
            "expected `{expected}` in `{message}` for kind {}",
            event.kind.as_u16()
        );
    }
}

#[test]
fn content_cap_is_byte_length_for_every_page_kind() {
    let (channel, page) = (Uuid::new_v4(), Uuid::new_v4());
    // 'é' is two bytes: 32_768 of them is exactly the cap, one more is over.
    let at_cap = "é".repeat(MAX_PAGE_CONTENT_BYTES / 2);
    let over_cap = format!("{at_cap}é");
    assert_eq!(at_cap.len(), MAX_PAGE_CONTENT_BYTES);
    // Built by hand: the SDK builders enforce the same cap and would refuse.
    let revision = |content: &str| {
        raw(
            52000,
            &[
                &["h", &channel.to_string()],
                &["d", &page.to_string()],
                &["title", "Plan"],
            ],
            content,
        )
    };
    parse_page_event(&revision(&at_cap)).expect("content at the cap is accepted");
    let message = expect_err(&revision(&over_cap));
    assert!(
        message.starts_with("invalid: page content exceeds maximum size of 65536 bytes"),
        "{message}"
    );

    let hex = "a".repeat(64);
    for kind in [52001, 52002] {
        let event = raw(
            kind,
            &[
                &["h", &channel.to_string()],
                &["d", &page.to_string()],
                &["base", &hex],
                &["e", &hex],
                &["status", "rejected"],
            ],
            &over_cap,
        );
        assert!(
            expect_err(&event).contains("exceeds maximum size"),
            "kind {kind}"
        );
    }
}

fn record(
    kind: u32,
    channel_tag: Uuid,
    page_tag: Uuid,
    stored_channel: Option<Uuid>,
) -> PageEventRecord {
    PageEventRecord {
        id: [7; 32],
        kind,
        author: [1; 32],
        channel_id: stored_channel,
        tags: vec![
            vec!["h".to_owned(), channel_tag.to_string()],
            vec!["d".to_owned(), page_tag.to_string()],
        ],
        content: String::new(),
    }
}

#[test]
fn reference_check_table() {
    let (channel, page) = (Uuid::new_v4(), Uuid::new_v4());
    let (other_channel, other_page) = (Uuid::new_v4(), Uuid::new_v4());
    let revision = |c, p, stored| record(KIND_PAGE_REVISION, c, p, stored);

    // The same page: accepted.
    assert_eq!(
        check_reference(
            &revision(channel, page, Some(channel)),
            "prev",
            KIND_PAGE_REVISION,
            channel,
            page
        ),
        Ok(())
    );
    // Another channel, by tags or by stored scope.
    for rec in [
        revision(other_channel, page, Some(other_channel)),
        revision(channel, page, Some(other_channel)),
        revision(channel, page, None),
    ] {
        assert_eq!(
            check_reference(&rec, "prev", KIND_PAGE_REVISION, channel, page),
            Err("invalid: prev event belongs to a different channel".to_owned())
        );
    }
    // Another page of the same channel.
    assert_eq!(
        check_reference(
            &revision(channel, other_page, Some(channel)),
            "base",
            KIND_PAGE_REVISION,
            channel,
            page
        ),
        Err("invalid: base event belongs to a different page".to_owned())
    );
    // Wrong kind in either direction.
    assert_eq!(
        check_reference(
            &record(KIND_PAGE_SUGGESTION, channel, page, Some(channel)),
            "prev",
            KIND_PAGE_REVISION,
            channel,
            page
        ),
        Err("invalid: prev must reference a page revision event".to_owned())
    );
    assert_eq!(
        check_reference(
            &revision(channel, page, Some(channel)),
            "e",
            KIND_PAGE_SUGGESTION,
            channel,
            page
        ),
        Err("invalid: e must reference a page suggestion event".to_owned())
    );
    // A stored event whose tags are not a valid identity.
    let mut broken = revision(channel, page, Some(channel));
    broken.tags.clear();
    assert_eq!(
        check_reference(&broken, "rev", KIND_PAGE_REVISION, channel, page),
        Err("invalid: rev event is not a valid page event".to_owned())
    );
}

/// A reference outside the event's channel is held back for an access check
/// (`store_page_event` describes it only to a reader of that channel); a failure
/// inside the channel is final. The hidden answer is exactly the one for an id
/// that was never stored.
#[test]
fn references_outside_the_channel_are_marked_foreign_with_the_missing_answer() {
    let (channel, page) = (Uuid::new_v4(), Uuid::new_v4());
    let (other_channel, other_page) = (Uuid::new_v4(), Uuid::new_v4());

    let foreign = |rec: &PageEventRecord, tag: &str, kind: u32| match check_scoped_reference(
        rec, tag, kind, channel, page,
    ) {
        Err(Refusal::Foreign {
            channel,
            shown,
            hidden,
        }) => (channel, shown, hidden),
        other => panic!("expected a foreign reference, got {other:?}"),
    };
    // Another channel: described to its readers, missing to everyone else.
    let (at, shown, hidden) = foreign(
        &record(KIND_PAGE_REVISION, other_channel, page, Some(other_channel)),
        "prev",
        KIND_PAGE_REVISION,
    );
    assert_eq!(at, Some(other_channel));
    assert_eq!(shown, "invalid: prev event belongs to a different channel");
    assert_eq!(hidden, "conflict: prev revision not found");
    // The wrong kind of event in another channel must not confirm that it exists.
    let (_, shown, hidden) = foreign(
        &record(
            KIND_PAGE_SUGGESTION,
            other_channel,
            page,
            Some(other_channel),
        ),
        "base",
        KIND_PAGE_REVISION,
    );
    assert_eq!(shown, "invalid: base must reference a page revision event");
    assert_eq!(hidden, "invalid: base event not found");
    // An event with no channel at all.
    let (at, _, hidden) = foreign(
        &record(KIND_PAGE_REVISION, channel, page, None),
        "rev",
        KIND_PAGE_REVISION,
    );
    assert_eq!(at, None);
    assert_eq!(hidden, "invalid: rev event not found");

    // Inside the channel the answer is final: the caller can read all of it.
    for (rec, tag, kind, expected) in [
        (
            record(KIND_PAGE_REVISION, channel, other_page, Some(channel)),
            "prev",
            KIND_PAGE_REVISION,
            "invalid: prev event belongs to a different page",
        ),
        (
            record(KIND_PAGE_SUGGESTION, channel, page, Some(channel)),
            "e",
            KIND_PAGE_REVISION,
            "invalid: e must reference a page revision event",
        ),
    ] {
        match check_scoped_reference(&rec, tag, kind, channel, page) {
            Err(Refusal::Ingest(IngestError::Rejected(message))) => assert_eq!(message, expected),
            other => panic!("expected a final rejection, got {other:?}"),
        }
    }
    assert!(check_scoped_reference(
        &record(KIND_PAGE_REVISION, channel, page, Some(channel)),
        "prev",
        KIND_PAGE_REVISION,
        channel,
        page
    )
    .is_ok());
}

/// A scoped token only describes the channels it reaches, even when the key's
/// owner is a member of others.
#[test]
fn a_scoped_token_describes_only_channels_it_reaches() {
    let (inside, outside) = (Uuid::new_v4(), Uuid::new_v4());
    assert!(token_allows(None, outside));
    assert!(token_allows(Some(&[inside]), inside));
    assert!(!token_allows(Some(&[inside]), outside));
    assert!(!token_allows(Some(&[]), inside));
}

#[test]
fn head_conflicts_map_to_stable_conflict_messages() {
    let head = [9_u8; 32];
    let message = |error| match error {
        IngestError::Rejected(message) => message,
        _ => panic!("expected a client rejection"),
    };
    assert_eq!(
        message(head_error(
            PageHeadError::Conflict {
                current_head: Some(head)
            },
            Some([1; 32])
        )),
        format!("conflict: stale prev (head {})", hex::encode(head))
    );
    assert_eq!(
        message(head_error(
            PageHeadError::Conflict {
                current_head: Some(head)
            },
            None
        )),
        format!("conflict: page already exists (head {})", hex::encode(head))
    );
    assert_eq!(
        message(head_error(
            PageHeadError::Conflict { current_head: None },
            Some([1; 32])
        )),
        "conflict: page does not exist"
    );
    assert_eq!(
        message(head_error(PageHeadError::Deleted, Some([1; 32]))),
        "conflict: page is deleted"
    );
}

#[test]
fn a_page_event_over_the_tag_cap_is_rejected() {
    let (channel, page) = (Uuid::new_v4(), Uuid::new_v4());
    let (channel, page) = (channel.to_string(), page.to_string());
    let pubkey = Keys::generate().public_key().to_hex();
    let mut tags: Vec<Vec<&str>> = vec![
        vec!["h", &channel],
        vec!["d", &page],
        vec!["title", "Plan"],
    ];
    tags.extend((0..MAX_PAGE_TAGS).map(|_| vec!["p", pubkey.as_str()]));
    let refs: Vec<&[&str]> = tags.iter().map(Vec::as_slice).collect();
    assert!(expect_err(&raw(KIND_PAGE_REVISION, &refs, "body")).contains("maximum of"));
}
