//! End-to-end tests for NIP-PG pages (kinds 52000-52002).
//!
//! These tests drive a running relay through its production ingest path
//! (WebSocket `EVENT`, `REQ`, `COUNT`), so a guard removed from the relay fails
//! here. They are `#[ignore]` so `cargo test` does not need a relay.
//!
//! # Running
//!
//! Start the relay (`just relay`, or `scripts/start-relay-for-tests.sh` as CI
//! does), then:
//!
//! ```text
//! cargo test -p buzz-test-client --test e2e_pages -- --ignored
//! ```
//!
//! `RELAY_URL` (default `ws://localhost:3000`) selects the relay and
//! `DATABASE_URL` the Postgres it uses; the community-isolation test needs the
//! latter to seed a second community.
//!
//! Every test creates its own channels and pages, so they run in parallel.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use buzz_core::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION};
use buzz_core::page::{MAX_PAGE_CONTENT_BYTES, MAX_PAGE_TITLE_BYTES};
use buzz_sdk::pages::{
    build_page_revision, build_page_suggestion, build_page_suggestion_resolution, PageEdit,
    PageResolution,
};
use buzz_test_client::{BuzzTestClient, RelayMessage};
use nostr::{
    Alphabet, Event, EventBuilder, EventId, Filter, Keys, Kind, SingleLetterTag, Tag, Timestamp,
};
use uuid::Uuid;

// -- Harness -------------------------------------------------------------------

fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3000".to_string())
}

fn sub_id(name: &str) -> String {
    format!("e2e-pages-{name}-{}", Uuid::new_v4())
}

/// Strictly increasing event timestamps inside the relay's ±15 minute window, so
/// history order never depends on how many events land in the same second.
fn tick() -> Timestamp {
    static NEXT: OnceLock<AtomicU64> = OnceLock::new();
    let next = NEXT.get_or_init(|| {
        let now = Timestamp::now().as_secs();
        AtomicU64::new(now - 600)
    });
    Timestamp::from(next.fetch_add(1, Ordering::SeqCst))
}

/// An authenticated connection that signs every event with its own keys.
struct Session {
    ws: BuzzTestClient,
    keys: Keys,
}

/// The relay's answer to a submitted event.
struct Reply {
    event: Event,
    accepted: bool,
    message: String,
}

impl Session {
    async fn connect(url: &str) -> Self {
        let keys = Keys::generate();
        let ws = BuzzTestClient::connect(url, &keys)
            .await
            .expect("connect and authenticate");
        Self { ws, keys }
    }

    async fn send(&mut self, builder: EventBuilder) -> Reply {
        let event = builder
            .custom_created_at(tick())
            .sign_with_keys(&self.keys)
            .expect("sign event");
        let ok = self
            .ws
            .send_event(event.clone())
            .await
            .expect("relay answers EVENT");
        Reply {
            event,
            accepted: ok.accepted,
            message: ok.message,
        }
    }

    /// Submit an event that must be stored and return it.
    async fn publish(&mut self, builder: EventBuilder) -> Event {
        let reply = self.send(builder).await;
        assert!(
            reply.accepted,
            "relay rejected a page event it must accept: {}",
            reply.message
        );
        reply.event
    }

    /// Submit an event that must be rejected; returns the relay's message and
    /// asserts the event is not retrievable afterwards.
    async fn expect_rejected(&mut self, builder: EventBuilder) -> String {
        let reply = self.send(builder).await;
        assert!(
            !reply.accepted,
            "relay accepted an event it must reject (kind {})",
            reply.event.kind.as_u16()
        );
        assert!(
            !self.exists(reply.event.id).await,
            "a rejected page event must not be stored: {}",
            reply.message
        );
        reply.message
    }

    async fn create_channel(&mut self, visibility: &str) -> Uuid {
        let channel = Uuid::new_v4();
        let event = EventBuilder::new(Kind::Custom(9007), "")
            .tags([
                Tag::parse(["h", &channel.to_string()]).unwrap(),
                Tag::parse(["name", &format!("pages-e2e-{channel}")]).unwrap(),
                Tag::parse(["channel_type", "stream"]).unwrap(),
                Tag::parse(["visibility", visibility]).unwrap(),
            ])
            .sign_with_keys(&self.keys)
            .unwrap();
        let ok = self.ws.send_event(event).await.expect("create channel");
        assert!(ok.accepted, "channel creation rejected: {}", ok.message);
        channel
    }

    async fn archive_channel(&mut self, channel: Uuid) {
        let event = EventBuilder::new(Kind::Custom(9002), "")
            .tags([
                Tag::parse(["h", &channel.to_string()]).unwrap(),
                Tag::parse(["archived", "true"]).unwrap(),
            ])
            .sign_with_keys(&self.keys)
            .unwrap();
        let ok = self.ws.send_event(event).await.expect("archive channel");
        assert!(ok.accepted, "archive rejected: {}", ok.message);
    }

    /// Run a one-shot REQ and return the stored events it matches, once each.
    ///
    /// A REQ registers its live subscription before it reads history, and the
    /// relay fans an event out after it has answered the writer's OK, so an event
    /// stored a moment ago can arrive both as history and as a live event before
    /// EOSE. NIP-01 allows that; clients (and this helper) dedupe by id.
    async fn query(&mut self, filter: Filter) -> Vec<Event> {
        let sid = sub_id("query");
        self.ws
            .subscribe(&sid, vec![filter])
            .await
            .expect("subscribe");
        let events = self
            .ws
            .collect_until_eose(&sid, Duration::from_secs(10))
            .await
            .expect("collect until EOSE");
        self.ws.close_subscription(&sid).await.expect("close");
        let mut seen = HashSet::new();
        events
            .into_iter()
            .filter(|event| seen.insert(event.id))
            .collect()
    }

    async fn count(&mut self, filter: Filter) -> u64 {
        let sid = sub_id("count");
        self.ws
            .send_raw(&serde_json::json!(["COUNT", sid, filter]))
            .await
            .expect("send COUNT");
        loop {
            match self
                .ws
                .recv_event(Duration::from_secs(10))
                .await
                .expect("COUNT answer")
            {
                RelayMessage::Count {
                    subscription_id,
                    count,
                } if subscription_id == sid => return count,
                RelayMessage::Closed {
                    subscription_id,
                    message,
                } if subscription_id == sid => panic!("COUNT refused: {message}"),
                _ => {}
            }
        }
    }

    async fn exists(&mut self, id: EventId) -> bool {
        !self.query(Filter::new().id(id)).await.is_empty()
    }

    /// Every revision of the page, as a client reads its history.
    async fn history(&mut self, channel: Uuid, page: Uuid) -> Vec<Event> {
        self.query(page_filter(&[KIND_PAGE_REVISION], channel, page))
            .await
    }
}

fn kind(kind: u32) -> Kind {
    Kind::Custom(kind as u16)
}

/// The filter a client uses to read one page: `#h` plus `#d` on page kinds.
fn page_filter(kinds: &[u32], channel: Uuid, page: Uuid) -> Filter {
    Filter::new()
        .kinds(kinds.iter().map(|k| kind(*k)))
        .custom_tag(SingleLetterTag::lowercase(Alphabet::H), channel.to_string())
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), page.to_string())
}

fn ids(events: &[Event]) -> HashSet<EventId> {
    events.iter().map(|event| event.id).collect()
}

fn revision(channel: Uuid, page: Uuid, title: &str, content: &str, edit: PageEdit) -> EventBuilder {
    build_page_revision(channel, page, title, content, edit).expect("build revision")
}

fn edit(prev: &Event) -> PageEdit {
    PageEdit::Edit { prev: prev.id }
}

fn suggest(channel: Uuid, page: Uuid, base: &Event, content: &str) -> EventBuilder {
    build_page_suggestion(channel, page, base.id, content).expect("build suggestion")
}

fn apply(prev: &Event, suggestion: &Event) -> PageEdit {
    PageEdit::ApplySuggestion {
        prev: prev.id,
        suggestion: suggestion.id,
    }
}

fn resolve(
    channel: Uuid,
    page: Uuid,
    suggestion: &Event,
    resolution: PageResolution,
) -> EventBuilder {
    build_page_suggestion_resolution(channel, page, suggestion.id, resolution)
        .expect("build resolution")
}

/// A page event built by hand, for events the SDK builders refuse to produce.
fn raw_event(event_kind: u32, tags: &[&[&str]], content: &str) -> EventBuilder {
    EventBuilder::new(kind(event_kind), content).tags(
        tags.iter()
            .map(|tag| Tag::parse(tag.iter().copied()).expect("tag")),
    )
}

// -- Tests ---------------------------------------------------------------------

/// Create, edit by two authors, read the history and the library back, and see a
/// resubmission answered as a duplicate.
#[tokio::test]
#[ignore]
async fn create_edit_history_over_req_and_count() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let mut bob = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let page = Uuid::new_v4();

    let r1 = alice
        .publish(revision(channel, page, "Plan", "# Plan", PageEdit::Create))
        .await;
    let r2 = bob
        .publish(revision(
            channel,
            page,
            "Plan v2",
            "# Plan\nmore",
            edit(&r1),
        ))
        .await;
    let r3 = alice
        .publish(revision(
            channel,
            page,
            "Plan v3",
            "# Plan\nmore\nmost",
            edit(&r2),
        ))
        .await;

    // History: exactly the three revisions, newest first, linked by prev tags.
    let history = alice.history(channel, page).await;
    assert_eq!(
        history.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![r3.id, r2.id, r1.id]
    );
    assert_eq!(history[0].content, "# Plan\nmore\nmost");
    assert_eq!(history[0].pubkey, alice.keys.public_key());
    assert_eq!(history[1].pubkey, bob.keys.public_key());
    let prev_of = |event: &Event| {
        event
            .tags
            .iter()
            .find(|tag| tag.as_slice().first().map(String::as_str) == Some("prev"))
            .and_then(|tag| tag.as_slice().get(1).cloned())
    };
    assert_eq!(prev_of(&history[0]), Some(r2.id.to_hex()));
    assert_eq!(prev_of(&history[1]), Some(r1.id.to_hex()));
    assert_eq!(prev_of(&history[2]), None);

    // COUNT answers the same filter exactly, and another page's id matches nothing.
    let filter = page_filter(&[KIND_PAGE_REVISION], channel, page);
    assert_eq!(bob.count(filter).await, 3);
    assert!(bob.history(channel, Uuid::new_v4()).await.is_empty());

    // The library read: revisions of the channel, no `#d`; the page's newest
    // revision is its head.
    let library = alice
        .query(
            Filter::new()
                .kind(kind(KIND_PAGE_REVISION))
                .custom_tag(SingleLetterTag::lowercase(Alphabet::H), channel.to_string()),
        )
        .await;
    assert_eq!(ids(&library), ids(&history));

    // A retried submission is a duplicate: accepted, not stored twice.
    let again = alice.ws.send_event(r3.clone()).await.expect("resubmit");
    assert!(again.accepted, "duplicate must be answered, not refused");
    assert!(again.message.starts_with("duplicate:"), "{}", again.message);
    assert_eq!(alice.history(channel, page).await.len(), 3);
}

/// A stale `prev` is a `conflict:` and stores nothing; so is a second create, a
/// revision of a page that does not exist, and a prev that was never stored.
#[tokio::test]
#[ignore]
async fn stale_prev_conflicts_and_stores_nothing() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let page = Uuid::new_v4();

    let r1 = alice
        .publish(revision(channel, page, "v1", "one", PageEdit::Create))
        .await;
    let r2 = alice
        .publish(revision(channel, page, "v2", "two", edit(&r1)))
        .await;

    // Based on the superseded r1.
    let message = alice
        .expect_rejected(revision(channel, page, "stale", "stale", edit(&r1)))
        .await;
    assert_eq!(
        message,
        format!("conflict: stale prev (head {})", r2.id.to_hex())
    );

    // A second "first revision" of an existing page.
    let message = alice
        .expect_rejected(revision(channel, page, "again", "again", PageEdit::Create))
        .await;
    assert_eq!(
        message,
        format!("conflict: page already exists (head {})", r2.id.to_hex())
    );

    // A prev that was never stored.
    let ghost = EventId::from_byte_array([7; 32]);
    let message = alice
        .expect_rejected(revision(
            channel,
            page,
            "ghost",
            "ghost",
            PageEdit::Edit { prev: ghost },
        ))
        .await;
    assert_eq!(message, "conflict: prev revision not found");

    // The history is untouched and the right prev still works.
    assert_eq!(
        ids(&alice.history(channel, page).await),
        HashSet::from([r1.id, r2.id])
    );
    alice
        .publish(revision(channel, page, "v3", "three", edit(&r2)))
        .await;
    assert_eq!(alice.history(channel, page).await.len(), 3);
}

/// Several writers race on one `prev`: exactly one wins, the rest conflict, and
/// only the winner is stored.
#[tokio::test]
#[ignore]
async fn concurrent_revisions_with_the_same_prev_have_one_winner() {
    let url = relay_url();
    let mut owner = Session::connect(&url).await;
    let channel = owner.create_channel("open").await;
    let page = Uuid::new_v4();
    let r1 = owner
        .publish(revision(channel, page, "v1", "one", PageEdit::Create))
        .await;

    const WRITERS: usize = 6;
    let barrier = Arc::new(tokio::sync::Barrier::new(WRITERS));
    let mut tasks = Vec::new();
    for n in 0..WRITERS {
        let (url, barrier, r1) = (url.clone(), barrier.clone(), r1.clone());
        tasks.push(tokio::spawn(async move {
            let mut writer = Session::connect(&url).await;
            let builder = revision(
                channel,
                page,
                &format!("racer {n}"),
                &format!("content from racer {n}"),
                edit(&r1),
            );
            barrier.wait().await;
            writer.send(builder).await
        }));
    }
    let mut accepted = Vec::new();
    let mut conflicts = 0;
    for task in tasks {
        let reply = task.await.expect("writer task");
        if reply.accepted {
            accepted.push(reply.event);
        } else {
            assert!(
                reply.message.starts_with("conflict:"),
                "a losing racer must get a conflict, got: {}",
                reply.message
            );
            conflicts += 1;
        }
    }
    assert_eq!(accepted.len(), 1, "exactly one racer may win");
    assert_eq!(conflicts, WRITERS - 1);

    let stored = ids(&owner.history(channel, page).await);
    assert_eq!(stored, HashSet::from([r1.id, accepted[0].id]));
    // And the winner is now the head: building on it works.
    owner
        .publish(revision(channel, page, "next", "next", edit(&accepted[0])))
        .await;
}

/// A revision whose title and content equal the head's is a no-op; changing
/// either one is a real edit.
#[tokio::test]
#[ignore]
async fn no_op_revisions_are_rejected() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let page = Uuid::new_v4();
    let r1 = alice
        .publish(revision(channel, page, "Plan", "body", PageEdit::Create))
        .await;

    let message = alice
        .expect_rejected(revision(channel, page, "Plan", "body", edit(&r1)))
        .await;
    assert_eq!(
        message,
        "invalid: no-op revision (title and content equal the page head)"
    );
    assert_eq!(alice.history(channel, page).await.len(), 1);

    let r2 = alice
        .publish(revision(channel, page, "Plan renamed", "body", edit(&r1)))
        .await;
    let r3 = alice
        .publish(revision(
            channel,
            page,
            "Plan renamed",
            "new body",
            edit(&r2),
        ))
        .await;
    assert_eq!(alice.history(channel, page).await.len(), 3);

    // Going back to an older revision's text is an edit, not a no-op.
    alice
        .publish(revision(channel, page, "Plan", "body", edit(&r3)))
        .await;
}

/// The 64 KiB content cap is a byte length; the 256-byte title and the blank
/// title rule hold too. Each is rejected, not truncated, and stores nothing.
#[tokio::test]
#[ignore]
async fn oversize_and_malformed_titles_and_content_are_rejected() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let (c, p) = (channel.to_string(), Uuid::new_v4());
    let page = p.to_string();
    let tags = |title: &str| -> Vec<Vec<String>> {
        vec![
            vec!["h".into(), c.clone()],
            vec!["d".into(), page.clone()],
            vec!["title".into(), title.into()],
        ]
    };
    let build = |title: &str, content: &str| {
        EventBuilder::new(kind(KIND_PAGE_REVISION), content)
            .tags(tags(title).into_iter().map(|t| Tag::parse(t).unwrap()))
    };

    // One byte over the cap, in ASCII and in two-byte characters ("é" x 32,769).
    let over_ascii = "a".repeat(MAX_PAGE_CONTENT_BYTES + 1);
    let over_multibyte = "é".repeat(MAX_PAGE_CONTENT_BYTES / 2 + 1);
    assert!(over_multibyte.chars().count() < MAX_PAGE_CONTENT_BYTES);
    for content in [&over_ascii, &over_multibyte] {
        let message = alice.expect_rejected(build("Big", content)).await;
        assert_eq!(
            message,
            format!(
                "invalid: page content exceeds maximum size of {MAX_PAGE_CONTENT_BYTES} bytes (got {})",
                content.len()
            )
        );
    }
    // Suggestions and resolutions are held to the same cap.
    let message = alice
        .expect_rejected(raw_event(
            KIND_PAGE_SUGGESTION,
            &[&["h", &c], &["d", &page], &["base", &"a".repeat(64)]],
            &over_ascii,
        ))
        .await;
    assert!(message.contains("exceeds maximum size"), "{message}");

    let message = alice
        .expect_rejected(build(&"t".repeat(MAX_PAGE_TITLE_BYTES + 1), "x"))
        .await;
    assert_eq!(message, "invalid: page title exceeds 256 bytes");
    let message = alice.expect_rejected(build("   ", "x")).await;
    assert_eq!(message, "invalid: page title must not be blank");

    // Exactly at the limits is accepted.
    let at_cap = "é".repeat(MAX_PAGE_CONTENT_BYTES / 2);
    assert_eq!(at_cap.len(), MAX_PAGE_CONTENT_BYTES);
    let created = alice
        .publish(build(&"t".repeat(MAX_PAGE_TITLE_BYTES), &at_cap))
        .await;
    assert_eq!(created.content.len(), MAX_PAGE_CONTENT_BYTES);
    assert_eq!(alice.history(channel, p).await.len(), 1);
}

/// An event without an `h` tag hits the channel gate, not a page rule; stray or
/// duplicate `h`/`d` tags and non-canonical ids are `invalid:`.
#[tokio::test]
#[ignore]
async fn page_events_need_exactly_one_canonical_h_and_d() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let (c, page) = (channel.to_string(), Uuid::new_v4().to_string());
    let hex = "a".repeat(64);

    // No h tag: the generic channel gate answers, for every page kind.
    for (page_kind, tags) in [
        (
            KIND_PAGE_REVISION,
            vec![vec!["d", page.as_str()], vec!["title", "t"]],
        ),
        (
            KIND_PAGE_SUGGESTION,
            vec![vec!["d", page.as_str()], vec!["base", hex.as_str()]],
        ),
        (
            KIND_PAGE_SUGGESTION_RESOLUTION,
            vec![
                vec!["d", page.as_str()],
                vec!["e", hex.as_str()],
                vec!["status", "rejected"],
            ],
        ),
    ] {
        let tags: Vec<&[&str]> = tags.iter().map(Vec::as_slice).collect();
        let message = alice.expect_rejected(raw_event(page_kind, &tags, "")).await;
        assert_eq!(
            message, "invalid: channel-scoped events must include an h tag",
            "kind {page_kind}"
        );
    }

    // Two d tags, an uppercase d, an uppercase h.
    let other = Uuid::new_v4().to_string();
    let message = alice
        .expect_rejected(raw_event(
            KIND_PAGE_REVISION,
            &[&["h", &c], &["d", &page], &["d", &other], &["title", "t"]],
            "",
        ))
        .await;
    assert_eq!(message, "invalid: page event must carry exactly one d tag");
    let message = alice
        .expect_rejected(raw_event(
            KIND_PAGE_REVISION,
            &[&["h", &c], &["d", &page.to_uppercase()], &["title", "t"]],
            "",
        ))
        .await;
    assert!(message.contains("canonical lowercase UUID"), "{message}");
    let message = alice
        .expect_rejected(raw_event(
            KIND_PAGE_REVISION,
            &[&["h", &c.to_uppercase()], &["d", &page], &["title", "t"]],
            "",
        ))
        .await;
    assert!(message.starts_with("invalid:"), "{message}");
}

/// A non-member of a private channel cannot write pages there, and an archived
/// channel accepts no page events.
#[tokio::test]
#[ignore]
async fn non_members_and_archived_channels_cannot_write_pages() {
    let url = relay_url();
    let mut owner = Session::connect(&url).await;
    let mut outsider = Session::connect(&url).await;
    let channel = owner.create_channel("private").await;
    let page = Uuid::new_v4();
    let r1 = owner
        .publish(revision(channel, page, "v1", "one", PageEdit::Create))
        .await;

    // Non-member: no revision, no suggestion, no resolution.
    let other_page = Uuid::new_v4();
    let reply = outsider
        .send(revision(
            channel,
            other_page,
            "intruder",
            "x",
            PageEdit::Create,
        ))
        .await;
    assert!(!reply.accepted);
    assert!(
        reply.message.contains("not a channel member"),
        "{}",
        reply.message
    );
    let reply = outsider.send(suggest(channel, page, &r1, "mine")).await;
    assert!(!reply.accepted);
    assert!(
        reply.message.contains("not a channel member"),
        "{}",
        reply.message
    );
    // The page is unreadable to the outsider, and was not created.
    assert!(owner.history(channel, other_page).await.is_empty());

    // Archived channel: even a member's page event is refused.
    owner.archive_channel(channel).await;
    let reply = owner
        .send(revision(channel, page, "v2", "two", edit(&r1)))
        .await;
    assert!(!reply.accepted);
    assert_eq!(reply.message, "invalid: channel is archived");
    let reply = owner.send(suggest(channel, page, &r1, "mine")).await;
    assert!(!reply.accepted);
    assert_eq!(reply.message, "invalid: channel is archived");
    assert!(!owner.exists(reply.event.id).await);
}

/// Every reference must exist and share the event's `(h, d)`: another channel,
/// another page of the same channel, or the wrong kind is `invalid:`.
#[tokio::test]
#[ignore]
async fn references_must_share_channel_and_page() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let c1 = alice.create_channel("open").await;
    let c2 = alice.create_channel("open").await;
    let (p1, p2) = (Uuid::new_v4(), Uuid::new_v4());

    let r1 = alice
        .publish(revision(c1, p1, "P1", "one", PageEdit::Create))
        .await;
    let s1 = alice.publish(suggest(c1, p1, &r1, "proposal")).await;
    let q1 = alice
        .publish(revision(c1, p2, "P2", "other page", PageEdit::Create))
        .await;
    let x1 = alice
        .publish(revision(
            c2,
            Uuid::new_v4(),
            "X",
            "elsewhere",
            PageEdit::Create,
        ))
        .await;

    // Another channel.
    let page_in_c2 = Uuid::new_v4();
    let message = alice
        .expect_rejected(revision(c2, page_in_c2, "x", "x", edit(&r1)))
        .await;
    assert_eq!(
        message,
        "invalid: prev event belongs to a different channel"
    );
    let message = alice
        .expect_rejected(suggest(c2, page_in_c2, &r1, "x"))
        .await;
    assert_eq!(
        message,
        "invalid: base event belongs to a different channel"
    );
    let message = alice
        .expect_rejected(resolve(c2, page_in_c2, &s1, PageResolution::Rejected))
        .await;
    assert_eq!(message, "invalid: e event belongs to a different channel");

    // Another page of the same channel.
    let message = alice
        .expect_rejected(revision(c1, p2, "P2b", "x", edit(&r1)))
        .await;
    assert_eq!(message, "invalid: prev event belongs to a different page");
    let message = alice.expect_rejected(suggest(c1, p2, &r1, "x")).await;
    assert_eq!(message, "invalid: base event belongs to a different page");
    let message = alice
        .expect_rejected(revision(c1, p2, "P2c", "y", apply(&q1, &s1)))
        .await;
    assert_eq!(
        message,
        "invalid: suggestion event belongs to a different page"
    );
    let message = alice
        .expect_rejected(resolve(c1, p2, &s1, PageResolution::Rejected))
        .await;
    assert_eq!(message, "invalid: e event belongs to a different page");
    let message = alice
        .expect_rejected(resolve(
            c1,
            p1,
            &s1,
            PageResolution::Accepted { revision: q1.id },
        ))
        .await;
    assert_eq!(message, "invalid: rev event belongs to a different page");

    // The wrong kind of event.
    let message = alice
        .expect_rejected(revision(c1, p1, "w", "w", PageEdit::Edit { prev: s1.id }))
        .await;
    assert_eq!(
        message,
        "invalid: prev must reference a page revision event"
    );
    let message = alice
        .expect_rejected(resolve(c1, p1, &r1, PageResolution::Rejected))
        .await;
    assert_eq!(message, "invalid: e must reference a page suggestion event");
    let message = alice.expect_rejected(suggest(c1, p1, &s1, "w")).await;
    assert_eq!(
        message,
        "invalid: base must reference a page revision event"
    );

    // The same page id in two channels is two independent pages.
    let twin = alice
        .publish(revision(c2, p1, "P1 in C2", "twin", PageEdit::Create))
        .await;
    assert_eq!(ids(&alice.history(c1, p1).await), HashSet::from([r1.id]));
    assert_eq!(ids(&alice.history(c2, p1).await), HashSet::from([twin.id]));
    assert!(alice.exists(x1.id).await);
}

/// The suggestion flow: a suggestion never moves the head; a stale base cannot
/// be applied; applying closes the suggestion even without a resolution; a
/// rejection closes it; double-closing is a conflict.
#[tokio::test]
#[ignore]
async fn suggestion_flow_stale_base_closure_and_resolutions() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let mut agent = Session::connect(&url).await;
    let mut bob = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let page = Uuid::new_v4();
    let head_of = |history: &[Event]| history.iter().max_by_key(|e| e.created_at).unwrap().id;

    let r1 = alice
        .publish(revision(channel, page, "Plan", "v1", PageEdit::Create))
        .await;
    // An agent suggests; the head does not move.
    let s1 = agent.publish(suggest(channel, page, &r1, "agent v1")).await;
    assert_eq!(head_of(&alice.history(channel, page).await), r1.id);

    // A human edit moves the head, so s1 (based on r1) is stale.
    let r2 = alice
        .publish(revision(channel, page, "Plan", "v2", edit(&r1)))
        .await;
    let message = alice
        .expect_rejected(revision(channel, page, "Plan", "agent v1", apply(&r2, &s1)))
        .await;
    assert_eq!(
        message,
        "conflict: suggestion is stale (its base is not the revision's prev)"
    );
    // Applying it on its old base satisfies the base rule but not the head.
    let message = alice
        .expect_rejected(revision(channel, page, "Plan", "agent v1", apply(&r1, &s1)))
        .await;
    assert_eq!(
        message,
        format!("conflict: stale prev (head {})", r2.id.to_hex())
    );
    assert_eq!(head_of(&alice.history(channel, page).await), r2.id);

    // A fresh suggestion against the new head applies in one event.
    let s2 = agent.publish(suggest(channel, page, &r2, "agent v2")).await;
    let r3 = alice
        .publish(revision(channel, page, "Plan", "agent v2", apply(&r2, &s2)))
        .await;
    assert_eq!(head_of(&alice.history(channel, page).await), r3.id);

    // s2 is closed by that revision alone. Applying it again, or rejecting it,
    // conflicts; only the audit `accepted` resolution is allowed, once.
    let message = alice
        .expect_rejected(revision(channel, page, "Plan", "again", apply(&r3, &s2)))
        .await;
    assert_eq!(message, "conflict: suggestion is already closed");
    let message = bob
        .expect_rejected(resolve(channel, page, &s2, PageResolution::Rejected))
        .await;
    assert_eq!(message, "conflict: suggestion is already applied");
    let message = bob
        .expect_rejected(resolve(
            channel,
            page,
            &s2,
            PageResolution::Accepted { revision: r3.id },
        ))
        .await;
    assert_eq!(
        message,
        "invalid: rev must be a revision published by the resolver"
    );
    let audit = alice
        .publish(resolve(
            channel,
            page,
            &s2,
            PageResolution::Accepted { revision: r3.id },
        ))
        .await;
    let message = alice
        .expect_rejected(resolve(
            channel,
            page,
            &s2,
            PageResolution::Accepted { revision: r3.id },
        ))
        .await;
    assert_eq!(message, "conflict: suggestion is already resolved");

    // A retried accept (the applying revision, or its audit resolution) is
    // answered as a duplicate, not as "already closed" or "already resolved":
    // the relay recognises an event it holds before applying state-dependent
    // rules, so a client that lost the first answer can safely resend.
    for retried in [&r3, &audit] {
        let again = alice
            .ws
            .send_event(retried.clone())
            .await
            .expect("resubmit");
        assert!(
            again.accepted && again.message.starts_with("duplicate:"),
            "a retried event must be a duplicate, got accepted={} `{}`",
            again.accepted,
            again.message
        );
    }

    // Any writer may reject an open suggestion; that closes it for good.
    let s3 = agent.publish(suggest(channel, page, &r3, "agent v3")).await;
    bob.publish(resolve(channel, page, &s3, PageResolution::Rejected))
        .await;
    let message = alice
        .expect_rejected(revision(channel, page, "Plan", "agent v3", apply(&r3, &s3)))
        .await;
    assert_eq!(message, "conflict: suggestion is already closed");
    let message = alice
        .expect_rejected(resolve(channel, page, &s3, PageResolution::Rejected))
        .await;
    assert_eq!(message, "conflict: suggestion is already resolved");

    // A suggestion equal to the head is already applied: applying it is a no-op,
    // and a rejection closes it.
    let s4 = agent.publish(suggest(channel, page, &r3, "agent v2")).await;
    let message = alice
        .expect_rejected(revision(channel, page, "Plan", "agent v2", apply(&r3, &s4)))
        .await;
    assert_eq!(
        message,
        "invalid: no-op revision (title and content equal the page head)"
    );
    alice
        .publish(resolve(channel, page, &s4, PageResolution::Rejected))
        .await;

    // Resolutions are shaped: a rejection carries no rev, an acceptance needs one.
    let s5 = agent.publish(suggest(channel, page, &r3, "agent v5")).await;
    let (c, p, hex) = (channel.to_string(), page.to_string(), s5.id.to_hex());
    let message = alice
        .expect_rejected(raw_event(
            KIND_PAGE_SUGGESTION_RESOLUTION,
            &[
                &["h", &c],
                &["d", &p],
                &["e", &hex],
                &["status", "accepted"],
            ],
            "",
        ))
        .await;
    assert_eq!(
        message,
        "invalid: an accepted resolution must carry a rev tag"
    );

    // The page's whole record, as one REQ reads it, and the history-only filter.
    let everything = alice
        .query(page_filter(
            &[
                KIND_PAGE_REVISION,
                KIND_PAGE_SUGGESTION,
                KIND_PAGE_SUGGESTION_RESOLUTION,
            ],
            channel,
            page,
        ))
        .await;
    let revisions = everything
        .iter()
        .filter(|e| e.kind == kind(KIND_PAGE_REVISION))
        .count();
    assert_eq!(revisions, 3, "r1, r2 and r3 only");
    assert_eq!(
        everything
            .iter()
            .filter(|e| e.kind == kind(KIND_PAGE_SUGGESTION))
            .count(),
        5
    );
    assert_eq!(alice.history(channel, page).await.len(), 3);
}

/// Two writers race to apply and to reject the same open suggestion: the page
/// writer lock makes the closure check and the write one decision, so exactly one
/// of them can close it. Many independent rounds, because one interleaving of two
/// requests proves little.
#[tokio::test]
#[ignore]
async fn applying_and_rejecting_one_suggestion_cannot_both_win() {
    const ROUNDS: usize = 24;
    let url = relay_url();
    let mut rounds = Vec::new();
    for _ in 0..ROUNDS {
        let url = url.clone();
        rounds.push(tokio::spawn(async move {
            let mut alice = Session::connect(&url).await;
            let channel = alice.create_channel("open").await;
            let page = Uuid::new_v4();
            let r1 = alice
                .publish(revision(channel, page, "Plan", "v1", PageEdit::Create))
                .await;
            let mut agent = Session::connect(&url).await;
            let s1 = agent.publish(suggest(channel, page, &r1, "v2")).await;

            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let apply_task = {
                let (url, barrier, r1, s1) = (url.clone(), barrier.clone(), r1.clone(), s1.clone());
                tokio::spawn(async move {
                    let mut applier = Session::connect(&url).await;
                    let builder = revision(channel, page, "Plan", "v2", apply(&r1, &s1));
                    barrier.wait().await;
                    applier.send(builder).await
                })
            };
            let reject_task = {
                let (url, barrier, s1) = (url.clone(), barrier.clone(), s1.clone());
                tokio::spawn(async move {
                    let mut rejecter = Session::connect(&url).await;
                    let builder = resolve(channel, page, &s1, PageResolution::Rejected);
                    barrier.wait().await;
                    rejecter.send(builder).await
                })
            };
            (
                apply_task.await.expect("apply task"),
                reject_task.await.expect("reject task"),
            )
        }));
    }
    for round in rounds {
        let (applied, rejected) = round.await.expect("round");
        assert!(
            applied.accepted ^ rejected.accepted,
            "exactly one of apply ({}) and reject ({}) may succeed",
            applied.message,
            rejected.message
        );
        let loser = if applied.accepted {
            &rejected
        } else {
            &applied
        };
        assert!(
            loser.message.starts_with("conflict:"),
            "the loser must get a conflict, got: {}",
            loser.message
        );
    }
}

/// A failing event insert must not move the head: the head advance and the event
/// store are one atomic operation. A NUL byte cannot be stored in the text
/// column, so the insert fails after validation; the next edit on the same prev
/// must still succeed.
#[tokio::test]
#[ignore]
async fn a_failed_insert_does_not_advance_the_head() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let page = Uuid::new_v4();
    let r1 = alice
        .publish(revision(channel, page, "v1", "one", PageEdit::Create))
        .await;

    let reply = alice
        .send(revision(channel, page, "v2", "bad\u{0}content", edit(&r1)))
        .await;
    assert!(!reply.accepted, "the insert must fail");
    assert!(
        reply.message.starts_with("error:"),
        "a storage failure is a server error, got: {}",
        reply.message
    );
    assert!(!alice.exists(reply.event.id).await);

    // The head is still r1: an edit on r1 is not stale.
    let r2 = alice
        .publish(revision(channel, page, "v2", "two", edit(&r1)))
        .await;
    assert_eq!(
        ids(&alice.history(channel, page).await),
        HashSet::from([r1.id, r2.id])
    );
}

/// NIP-PG rule 8: a quiet page's history is complete in a channel flooded with
/// other pages' revisions, even with a REQ `limit` below the flood; COUNT is exact.
#[tokio::test]
#[ignore]
async fn quiet_page_history_is_exact_in_a_flooded_channel() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;

    let quiet = Uuid::new_v4();
    let q1 = alice
        .publish(revision(channel, quiet, "Quiet", "1", PageEdit::Create))
        .await;
    let q2 = alice
        .publish(revision(channel, quiet, "Quiet", "2", edit(&q1)))
        .await;
    let q3 = alice
        .publish(revision(channel, quiet, "Quiet", "3", edit(&q2)))
        .await;
    // Twelve other pages with two revisions each: 24 newer events.
    for n in 0..12 {
        let page = Uuid::new_v4();
        let first = alice
            .publish(revision(
                channel,
                page,
                &format!("busy {n}"),
                "a",
                PageEdit::Create,
            ))
            .await;
        alice
            .publish(revision(
                channel,
                page,
                &format!("busy {n}"),
                "b",
                edit(&first),
            ))
            .await;
    }

    let limit = 5;
    let history = alice
        .query(page_filter(&[KIND_PAGE_REVISION], channel, quiet).limit(limit))
        .await;
    assert_eq!(
        history.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![q3.id, q2.id, q1.id],
        "the whole history, newest first, despite limit {limit}"
    );
    assert_eq!(
        alice
            .count(page_filter(&[KIND_PAGE_REVISION], channel, quiet))
            .await,
        3
    );

    // Falsifiability of the scenario itself: the channel-wide window of the same
    // size is full of other pages' revisions and holds none of the quiet page.
    let window = alice
        .query(
            Filter::new()
                .kind(kind(KIND_PAGE_REVISION))
                .custom_tag(SingleLetterTag::lowercase(Alphabet::H), channel.to_string())
                .limit(limit),
        )
        .await;
    assert_eq!(window.len(), limit);
    assert!(window
        .iter()
        .all(|e| ![q1.id, q2.id, q3.id].contains(&e.id)));
}

/// Page events fan out to live subscribers of the page, and only to them.
#[tokio::test]
#[ignore]
async fn page_events_fan_out_to_live_subscribers() {
    let url = relay_url();
    let mut writer = Session::connect(&url).await;
    let mut watcher = Session::connect(&url).await;
    let channel = writer.create_channel("open").await;
    let page = Uuid::new_v4();
    let elsewhere = Uuid::new_v4();
    let all_kinds = [
        KIND_PAGE_REVISION,
        KIND_PAGE_SUGGESTION,
        KIND_PAGE_SUGGESTION_RESOLUTION,
    ];

    let watching = sub_id("watch-page");
    let other = sub_id("watch-other");
    watcher
        .ws
        .subscribe(&watching, vec![page_filter(&all_kinds, channel, page)])
        .await
        .expect("subscribe to the page");
    watcher
        .ws
        .collect_until_eose(&watching, Duration::from_secs(10))
        .await
        .expect("EOSE for the page subscription");
    watcher
        .ws
        .subscribe(&other, vec![page_filter(&all_kinds, channel, elsewhere)])
        .await
        .expect("subscribe to another page");
    watcher
        .ws
        .collect_until_eose(&other, Duration::from_secs(10))
        .await
        .expect("EOSE for the other subscription");

    let r1 = writer
        .publish(revision(channel, page, "Live", "one", PageEdit::Create))
        .await;
    let s1 = writer.publish(suggest(channel, page, &r1, "two")).await;
    let res = writer
        .publish(resolve(channel, page, &s1, PageResolution::Rejected))
        .await;

    let mut seen = HashSet::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while seen.len() < 3 {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .filter(|d| !d.is_zero())
            .expect("timed out waiting for live page events");
        if let RelayMessage::Event {
            subscription_id,
            event,
        } = watcher.ws.recv_event(remaining).await.expect("live event")
        {
            assert_eq!(
                subscription_id, watching,
                "an event for another page must not reach this subscription"
            );
            seen.insert(event.id);
        }
    }
    assert_eq!(seen, HashSet::from([r1.id, s1.id, res.id]));

    // Nothing more arrives for either subscription.
    assert!(
        watcher
            .ws
            .recv_event(Duration::from_millis(500))
            .await
            .is_err(),
        "no further events expected"
    );
}

/// Deleting a page event (NIP-09 by its author, kind 9005 by a channel admin)
/// repairs the page head in the same transaction: the index never names a
/// deleted revision, and the page is gone once no live revision is left.
#[tokio::test]
#[ignore]
async fn deleting_page_events_repairs_the_head() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let mut bob = Session::connect(&url).await;
    let channel = alice.create_channel("open").await;
    let page = Uuid::new_v4();

    let r1 = alice
        .publish(revision(channel, page, "v1", "one", PageEdit::Create))
        .await;
    let r2 = bob
        .publish(revision(channel, page, "v2", "two", edit(&r1)))
        .await;
    let s1 = bob.publish(suggest(channel, page, &r2, "three")).await;

    // Bob deletes his own head revision (NIP-09): the head falls back to r1.
    let deletion = |target: &Event, kind_number: u16| {
        EventBuilder::new(Kind::Custom(kind_number), "").tags([
            Tag::parse(["h", &channel.to_string()]).unwrap(),
            Tag::parse(["e", &target.id.to_hex()]).unwrap(),
        ])
    };
    bob.publish(deletion(&r2, 5)).await;
    assert_eq!(
        ids(&alice.history(channel, page).await),
        HashSet::from([r1.id])
    );
    // r2 no longer exists, so an edit based on it names an unknown prev; an edit
    // on the repaired head is accepted.
    let message = alice
        .expect_rejected(revision(channel, page, "v3", "three", edit(&r2)))
        .await;
    assert_eq!(message, "conflict: prev revision not found");
    let r3 = alice
        .publish(revision(channel, page, "v3", "three", edit(&r1)))
        .await;

    // The channel owner removes bob's suggestion with kind 9005.
    alice.publish(deletion(&s1, 9005)).await;
    let suggestions = alice
        .query(page_filter(&[KIND_PAGE_SUGGESTION], channel, page))
        .await;
    assert!(suggestions.is_empty(), "the suggestion must be deleted");

    // Deleting every live revision removes the page: its id can be created anew.
    alice.publish(deletion(&r3, 5)).await;
    alice.publish(deletion(&r1, 5)).await;
    assert!(alice.history(channel, page).await.is_empty());
    let reborn = alice
        .publish(revision(channel, page, "again", "again", PageEdit::Create))
        .await;
    assert_eq!(
        ids(&alice.history(channel, page).await),
        HashSet::from([reborn.id])
    );
}

/// NIP-PG rule 9: a `#h`-less filter for revisions is scoped to the channels the
/// reader can access, newest first, and pages with `until`. The author filter
/// only keeps this test's events apart from other tests' pages in open channels;
/// the query path is the same as the plain `{"kinds":[52000]}` library read.
#[tokio::test]
#[ignore]
async fn hless_library_read_is_access_scoped_newest_first_and_until_paged() {
    let url = relay_url();
    let mut alice = Session::connect(&url).await;
    let mut bob = Session::connect(&url).await;
    let open = alice.create_channel("open").await;
    let secret = alice.create_channel("private").await;

    // Interleave open and private pages so a window crosses both.
    let mut open_events = Vec::new();
    let mut secret_events = Vec::new();
    for n in 0..4 {
        let page = Uuid::new_v4();
        let first = alice
            .publish(revision(
                open,
                page,
                &format!("open {n}"),
                "a",
                PageEdit::Create,
            ))
            .await;
        let second = alice
            .publish(revision(
                open,
                page,
                &format!("open {n}"),
                "b",
                edit(&first),
            ))
            .await;
        open_events.extend([first, second]);
        let hidden = alice
            .publish(revision(
                secret,
                Uuid::new_v4(),
                &format!("secret {n}"),
                "s",
                PageEdit::Create,
            ))
            .await;
        secret_events.push(hidden);
    }
    // Let the post-commit fan-out of the last writes settle, so a read's history
    // order is not interleaved with a live duplicate of a just-written event.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let alice_pubkey = alice.keys.public_key();
    let library = |limit: usize, until: Option<Timestamp>| {
        let filter = Filter::new()
            .kind(kind(KIND_PAGE_REVISION))
            .author(alice_pubkey)
            .limit(limit);
        match until {
            Some(until) => filter.until(until),
            None => filter,
        }
    };

    // The member sees every page; the non-member sees only the open channel's.
    let all = alice.query(library(100, None)).await;
    assert_eq!(all.len(), open_events.len() + secret_events.len());
    let seen_by_bob = bob.query(library(100, None)).await;
    assert_eq!(ids(&seen_by_bob), ids(&open_events));
    assert!(
        seen_by_bob
            .iter()
            .all(|e| !ids(&secret_events).contains(&e.id)),
        "a private channel's pages must not reach a non-member"
    );

    // Newest first.
    let times: Vec<_> = seen_by_bob.iter().map(|e| e.created_at).collect();
    assert!(
        times.windows(2).all(|pair| pair[0] >= pair[1]),
        "library must be newest first: {times:?}"
    );

    // Page the non-member's library three events at a time with `until`; a
    // boundary event may repeat, so dedupe by id.
    let mut collected: Vec<Event> = Vec::new();
    let mut until = None;
    for _ in 0..10 {
        let window = bob.query(library(3, until)).await;
        let Some(oldest) = window.last().map(|e| e.created_at) else {
            break;
        };
        let before = collected.len();
        for event in window {
            if !collected.iter().any(|seen| seen.id == event.id) {
                collected.push(event);
            }
        }
        if collected.len() == before {
            break;
        }
        until = Some(oldest);
    }
    assert_eq!(
        ids(&collected),
        ids(&open_events),
        "paging must reach every page"
    );
}

// -- Community isolation --------------------------------------------------------

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://buzz:buzz_dev@localhost:5432/buzz".to_string() // sadscan:disable np.postgres.1
    })
}

async fn ensure_community(host: &str) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url())
        .await
        .expect("connect to e2e Postgres");
    sqlx::query(
        "INSERT INTO communities (id, host) VALUES ($1, $2) ON CONFLICT (lower(host)) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(host)
    .execute(&pool)
    .await
    .unwrap_or_else(|e| panic!("seed community {host}: {e}"));
}

/// The same relay addressed by a second `Host`: `localhost` and `127.0.0.1` reach
/// one process but are two communities.
fn second_host_url(url: &str) -> String {
    let parsed = url::Url::parse(url).expect("RELAY_URL");
    let host_b = if parsed.host_str() == Some("127.0.0.1") {
        "localhost"
    } else {
        "127.0.0.1"
    };
    let mut other = parsed.clone();
    other.set_host(Some(host_b)).expect("set host");
    other.as_str().trim_end_matches('/').to_string()
}

/// Pages are per community: the same channel and page ids in two communities are
/// two pages, and neither can reference or read the other's events.
#[tokio::test]
#[ignore]
async fn pages_are_isolated_between_communities() {
    let url_a = relay_url();
    let url_b = second_host_url(&url_a);
    let authority = |url: &str| {
        let parsed = url::Url::parse(url).expect("url");
        match parsed.port() {
            Some(port) => format!("{}:{port}", parsed.host_str().expect("host")),
            None => parsed.host_str().expect("host").to_string(),
        }
    };
    ensure_community(&authority(&url_b)).await;

    let mut alice = Session::connect(&url_a).await;
    let mut bob = Session::connect(&url_b).await;
    let channel = Uuid::new_v4();
    let page = Uuid::new_v4();

    // The same channel id, created independently in each community.
    for (session, label) in [(&mut alice, "A"), (&mut bob, "B")] {
        let event = EventBuilder::new(Kind::Custom(9007), "")
            .tags([
                Tag::parse(["h", &channel.to_string()]).unwrap(),
                Tag::parse(["name", &format!("iso-{label}-{channel}")]).unwrap(),
                Tag::parse(["channel_type", "stream"]).unwrap(),
                Tag::parse(["visibility", "open"]).unwrap(),
            ])
            .sign_with_keys(&session.keys)
            .unwrap();
        let ok = session.ws.send_event(event).await.expect("create channel");
        assert!(ok.accepted, "community {label}: {}", ok.message);
    }

    let a1 = alice
        .publish(revision(channel, page, "In A", "alpha", PageEdit::Create))
        .await;
    // The first revision of the same (h, d) in community B is a fresh page, not a
    // conflict with community A's.
    let b1 = bob
        .publish(revision(channel, page, "In B", "beta", PageEdit::Create))
        .await;
    assert_ne!(a1.id, b1.id);
    assert_eq!(
        ids(&alice.history(channel, page).await),
        HashSet::from([a1.id])
    );
    assert_eq!(
        ids(&bob.history(channel, page).await),
        HashSet::from([b1.id])
    );

    // B cannot see A's events by id, nor build on them: the reference is simply
    // unknown there.
    assert!(!bob.exists(a1.id).await);
    let message = bob
        .expect_rejected(revision(channel, page, "x", "x", edit(&a1)))
        .await;
    assert_eq!(message, "conflict: prev revision not found");
    let message = bob.expect_rejected(suggest(channel, page, &a1, "x")).await;
    assert_eq!(message, "invalid: base event not found");

    // And B's edits never touch A's head: A can still build on a1.
    let b2 = bob
        .publish(revision(channel, page, "In B 2", "beta2", edit(&b1)))
        .await;
    alice
        .publish(revision(channel, page, "In A 2", "alpha2", edit(&a1)))
        .await;
    assert!(!alice.exists(b2.id).await);
}
