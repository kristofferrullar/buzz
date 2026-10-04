//! Parsing of `PAGE_REVISION` events into index fields. No infrastructure needed.

use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp};

use super::*;

const CHANNEL: &str = "0b1e5c6e-1f6a-4f43-8f3f-2f0a6f7f6a10";
const PAGE: &str = "5d3f6a2e-8c1b-4a39-9d57-7b0f4f6f2c21";
const PREV: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

fn tags(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(|s| (*s).to_owned()).collect())
        .collect()
}

fn parse(rows: &[&[&str]]) -> std::result::Result<PageRevisionMeta, PageRevisionError> {
    parse_revision(
        [7; 32],
        [9; 32],
        DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp"),
        &tags(rows),
    )
}

#[test]
fn first_revision_parses_without_prev() {
    let meta = parse(&[&["h", CHANNEL], &["d", PAGE], &["title", "Q4 plan"]]).expect("valid");
    assert_eq!(meta.channel_id.to_string(), CHANNEL);
    assert_eq!(meta.page_id.to_string(), PAGE);
    assert_eq!(meta.title, "Q4 plan");
    assert_eq!(meta.prev, None);
    assert_eq!(meta.event_id, [7; 32]);
    assert_eq!(meta.author, [9; 32]);
}

#[test]
fn later_revision_parses_prev_and_ignores_unrelated_tags() {
    let meta = parse(&[
        &["h", CHANNEL],
        &["d", PAGE],
        &["title", "Q4 plan"],
        &["prev", PREV],
        &["suggestion", PREV],
        &["t", "noise"],
    ])
    .expect("valid");
    assert_eq!(meta.prev.map(hex::encode).as_deref(), Some(PREV));
}

#[test]
fn malformed_revisions_are_rejected_with_the_offending_tag() {
    let long_title = "x".repeat(MAX_PAGE_TITLE_BYTES + 1);
    let max_title = "y".repeat(MAX_PAGE_TITLE_BYTES);
    let upper_channel = CHANNEL.to_uppercase();
    let upper_prev = PREV.to_uppercase();
    let simple_page = PAGE.replace('-', "");
    let cases: Vec<(&str, Vec<Vec<String>>, bool)> = vec![
        ("missing h", tags(&[&["d", PAGE], &["title", "t"]]), false),
        (
            "two h",
            tags(&[
                &["h", CHANNEL],
                &["h", CHANNEL],
                &["d", PAGE],
                &["title", "t"],
            ]),
            false,
        ),
        (
            "missing d",
            tags(&[&["h", CHANNEL], &["title", "t"]]),
            false,
        ),
        (
            "two d",
            tags(&[&["h", CHANNEL], &["d", PAGE], &["d", PAGE], &["title", "t"]]),
            false,
        ),
        (
            "missing title",
            tags(&[&["h", CHANNEL], &["d", PAGE]]),
            false,
        ),
        (
            "two titles",
            tags(&[
                &["h", CHANNEL],
                &["d", PAGE],
                &["title", "a"],
                &["title", "b"],
            ]),
            false,
        ),
        (
            "blank title",
            tags(&[&["h", CHANNEL], &["d", PAGE], &["title", " \t "]]),
            false,
        ),
        (
            "empty title",
            tags(&[&["h", CHANNEL], &["d", PAGE], &["title", ""]]),
            false,
        ),
        (
            "oversize title",
            tags(&[&["h", CHANNEL], &["d", PAGE], &["title", &long_title]]),
            false,
        ),
        (
            "title at the limit is fine",
            tags(&[&["h", CHANNEL], &["d", PAGE], &["title", &max_title]]),
            true,
        ),
        (
            "non-uuid page",
            tags(&[&["h", CHANNEL], &["d", "not-a-uuid"], &["title", "t"]]),
            false,
        ),
        (
            "uppercase channel is not canonical",
            tags(&[&["h", &upper_channel], &["d", PAGE], &["title", "t"]]),
            false,
        ),
        (
            "simple-form page uuid is not canonical",
            tags(&[&["h", CHANNEL], &["d", &simple_page], &["title", "t"]]),
            false,
        ),
        (
            "short prev",
            tags(&[
                &["h", CHANNEL],
                &["d", PAGE],
                &["title", "t"],
                &["prev", "abcd"],
            ]),
            false,
        ),
        (
            "uppercase prev",
            tags(&[
                &["h", CHANNEL],
                &["d", PAGE],
                &["title", "t"],
                &["prev", &upper_prev],
            ]),
            false,
        ),
        (
            "two prevs",
            tags(&[
                &["h", CHANNEL],
                &["d", PAGE],
                &["title", "t"],
                &["prev", PREV],
                &["prev", PREV],
            ]),
            false,
        ),
        (
            "valueless h",
            tags(&[&["h"], &["d", PAGE], &["title", "t"]]),
            false,
        ),
    ];
    for (name, rows, should_parse) in cases {
        let result = parse_revision(
            [1; 32],
            [2; 32],
            DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp"),
            &rows,
        );
        assert_eq!(result.is_ok(), should_parse, "{name}: {result:?}");
    }
}

#[test]
fn from_event_reads_a_signed_revision_and_rejects_other_kinds() {
    let keys = Keys::generate();
    let revision = EventBuilder::new(Kind::Custom(KIND_PAGE_REVISION as u16), "# body")
        .tags([
            Tag::parse(["h", CHANNEL]).expect("h tag"),
            Tag::parse(["d", PAGE]).expect("d tag"),
            Tag::parse(["title", "Plan"]).expect("title tag"),
            Tag::parse(["prev", PREV]).expect("prev tag"),
        ])
        .custom_created_at(Timestamp::from(1_700_000_123))
        .sign_with_keys(&keys)
        .expect("sign");
    let meta = PageRevisionMeta::from_event(&revision).expect("valid revision");
    assert_eq!(meta.event_id, *revision.id.as_bytes());
    assert_eq!(meta.author, keys.public_key().to_bytes());
    assert_eq!(meta.created_at.timestamp(), 1_700_000_123);
    assert_eq!(meta.title, "Plan");

    let suggestion = EventBuilder::new(Kind::Custom(KIND_PAGE_SUGGESTION as u16), "# body")
        .tags([
            Tag::parse(["h", CHANNEL]).expect("h tag"),
            Tag::parse(["d", PAGE]).expect("d tag"),
        ])
        .sign_with_keys(&keys)
        .expect("sign");
    assert_eq!(
        PageRevisionMeta::from_event(&suggestion),
        Err(PageRevisionError::NotARevision(KIND_PAGE_SUGGESTION))
    );
}
