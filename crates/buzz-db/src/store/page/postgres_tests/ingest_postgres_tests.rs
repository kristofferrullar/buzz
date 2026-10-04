//! Postgres-backed tests of the ingest primitives the relay composes: the
//! `d_tag` column that makes `#d` page queries exact, suggestion closure,
//! reference reads, the page writer lock, and deleting a page event while
//! repairing the head.

use std::time::{Duration, Instant};

use buzz_core::kind::{KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION};

use super::*;
use crate::event::EventQuery;

// -- Fixtures ------------------------------------------------------------------

fn page_event(keys: &Keys, kind: u32, tags: Vec<Vec<String>>, content: &str, secs: u64) -> Event {
    let tags: Vec<Tag> = tags
        .into_iter()
        .map(|tag| Tag::parse(tag).expect("tag"))
        .collect();
    EventBuilder::new(Kind::Custom(kind as u16), content)
        .tags(tags)
        .custom_created_at(Timestamp::from(secs))
        .sign_with_keys(keys)
        .expect("sign page event")
}

fn pair(name: &str, value: impl ToString) -> Vec<String> {
    vec![name.to_owned(), value.to_string()]
}

fn suggestion(keys: &Keys, channel: Uuid, page: Uuid, base: &Event, secs: u64) -> Event {
    page_event(
        keys,
        KIND_PAGE_SUGGESTION,
        vec![
            pair("h", channel),
            pair("d", page),
            pair("base", base.id.to_hex()),
        ],
        "proposed",
        secs,
    )
}

fn resolution(
    keys: &Keys,
    channel: Uuid,
    page: Uuid,
    suggestion: &Event,
    status: &str,
    secs: u64,
) -> Event {
    page_event(
        keys,
        KIND_PAGE_SUGGESTION_RESOLUTION,
        vec![
            pair("h", channel),
            pair("d", page),
            pair("e", suggestion.id.to_hex()),
            pair("status", status),
        ],
        "",
        secs,
    )
}

fn applying_revision(
    keys: &Keys,
    channel: Uuid,
    page: Uuid,
    prev: &Event,
    suggestion: &Event,
    secs: u64,
) -> Event {
    page_event(
        keys,
        KIND_PAGE_REVISION,
        vec![
            pair("h", channel),
            pair("d", page),
            pair("title", "applied"),
            pair("prev", prev.id.to_hex()),
            pair("suggestion", suggestion.id.to_hex()),
        ],
        "applied content",
        secs,
    )
}

/// Store a page event the way the relay does: through
/// `insert_page_event_in_transaction`, moving the head for a revision, in one
/// transaction.
async fn ingest(db: &Db, community: CommunityId, event: &Event) {
    let kind = u32::from(event.kind.as_u16());
    let channel = event
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(String::as_str) == Some("h"))
        .and_then(|tag| tag.as_slice().get(1))
        .and_then(|value| Uuid::parse_str(value).ok())
        .expect("page event has an h tag");
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let (_, inserted) = insert_page_event_in_transaction(&mut tx, community, event, channel)
        .await
        .expect("insert page event");
    assert!(inserted, "test events are unique");
    if kind == KIND_PAGE_REVISION {
        record_page_revision_in_transaction(&mut tx, community, &meta(event))
            .await
            .expect("move head");
    }
    tx.commit().await.expect("commit");
}

async fn d_tag_of(db: &Db, community: CommunityId, event: &Event) -> Option<String> {
    sqlx::query_scalar("SELECT d_tag FROM events WHERE community_id = $1 AND id = $2")
        .bind(community.as_uuid())
        .bind(event.id.as_bytes().as_slice())
        .fetch_one(&db.pool)
        .await
        .expect("read d_tag")
}

async fn state_of(
    db: &Db,
    community: CommunityId,
    channel: Uuid,
    page: Uuid,
    suggestion: &Event,
) -> SuggestionState {
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    suggestion_state_in_transaction(&mut tx, community, channel, page, &id32(suggestion))
        .await
        .expect("suggestion state")
}

fn history_query(community: CommunityId, channel: Uuid, page: Uuid, limit: i64) -> EventQuery {
    EventQuery {
        channel_id: Some(channel),
        kinds: Some(vec![KIND_PAGE_REVISION as i32]),
        d_tag: Some(page.to_string()),
        limit: Some(limit),
        ..EventQuery::for_community(community)
    }
}

// -- d_tag and exact #d queries ------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn every_page_kind_stores_its_page_id_in_the_d_tag_column() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();

    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    let s1 = suggestion(&keys, channel, page, &r1, 1_100);
    let res = resolution(&keys, channel, page, &s1, "rejected", 1_200);
    ingest(&db, community, &r1).await;
    ingest(&db, community, &s1).await;
    ingest(&db, community, &res).await;
    for event in [&r1, &s1, &res] {
        assert_eq!(
            d_tag_of(&db, community, event).await,
            Some(page.to_string()),
            "kind {}",
            event.kind.as_u16()
        );
    }

    // Other regular kinds keep a NULL d_tag even if they carry a `d` tag.
    let note = page_event(&keys, 9, vec![pair("d", page)], "chat", 1_300);
    crate::event::insert_event(&db.pool, community, &note, Some(channel))
        .await
        .expect("store note");
    assert_eq!(d_tag_of(&db, community, &note).await, None);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_quiet_pages_history_is_complete_in_a_channel_flooded_by_other_pages() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();

    // The quiet page's three revisions are the OLDEST events in the channel.
    let quiet = Uuid::new_v4();
    let q1 = revision(&keys, channel, quiet, None, "q1", 1_000);
    let q2 = revision(&keys, channel, quiet, Some(&q1), "q2", 1_001);
    let q3 = revision(&keys, channel, quiet, Some(&q2), "q3", 1_002);
    for event in [&q1, &q2, &q3] {
        ingest(&db, community, event).await;
    }
    // Twenty other pages, each with two revisions, all newer than the quiet page.
    for n in 0..20_u64 {
        let page = Uuid::new_v4();
        let first = revision(&keys, channel, page, None, "busy", 2_000 + 2 * n);
        let second = revision(&keys, channel, page, Some(&first), "busier", 2_001 + 2 * n);
        ingest(&db, community, &first).await;
        ingest(&db, community, &second).await;
    }

    // The limit (5) is far below the 40 newer events, yet the exact `#d` pushdown
    // returns the whole history, newest first.
    let limit = 5;
    let rows =
        crate::event::query_events(&db.pool, &history_query(community, channel, quiet, limit))
            .await
            .expect("history");
    let ids: Vec<_> = rows.iter().map(|row| row.event.id).collect();
    assert_eq!(ids, vec![q3.id, q2.id, q1.id]);

    // Falsifiability: without the `d_tag` predicate the same limit is consumed
    // entirely by the other pages, so post-filtering would have lost the history.
    let unpushed = EventQuery {
        d_tag: None,
        ..history_query(community, channel, quiet, limit)
    };
    let window = crate::event::query_events(&db.pool, &unpushed)
        .await
        .expect("unpushed window");
    assert_eq!(window.len(), limit as usize);
    assert!(
        window
            .iter()
            .all(|row| ![q1.id, q2.id, q3.id].contains(&row.event.id)),
        "the flood must fill the window, or this test proves nothing"
    );

    // COUNT is exact too, and several page ids are answered as one OR.
    assert_eq!(
        crate::event::count_events(&db.pool, &history_query(community, channel, quiet, limit))
            .await
            .expect("count"),
        3
    );
    let busy_page = revision(&keys, channel, Uuid::new_v4(), None, "extra", 9_000);
    ingest(&db, community, &busy_page).await;
    let busy_id = meta(&busy_page).page_id;
    let either = EventQuery {
        d_tag: None,
        d_tags: Some(vec![quiet.to_string(), busy_id.to_string()]),
        ..history_query(community, channel, quiet, 100)
    };
    assert_eq!(
        crate::event::count_events(&db.pool, &either)
            .await
            .expect("count either"),
        4
    );
}

// -- Suggestion closure --------------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_suggestion_is_closed_only_by_a_live_exact_resolution_or_applying_revision() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let other_channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    ingest(&db, community, &r1).await;
    let s1 = suggestion(&keys, channel, page, &r1, 1_100);
    ingest(&db, community, &s1).await;

    let open = state_of(&db, community, channel, page, &s1).await;
    assert_eq!(
        open,
        SuggestionState {
            resolved: false,
            applied: false
        }
    );
    assert!(!open.is_closed());

    // A decoy tag naming the suggestion at a position other than `e`, and a
    // resolution of the same suggestion id filed under another channel, do not
    // close it.
    let decoy = page_event(
        &keys,
        KIND_PAGE_SUGGESTION_RESOLUTION,
        vec![
            pair("h", channel),
            pair("d", page),
            vec!["zz".to_owned(), "e".to_owned(), s1.id.to_hex()],
            pair("status", "rejected"),
        ],
        "",
        1_150,
    );
    ingest(&db, community, &decoy).await;
    let elsewhere = resolution(&keys, other_channel, page, &s1, "rejected", 1_160);
    ingest(&db, community, &elsewhere).await;
    assert!(!state_of(&db, community, channel, page, &s1)
        .await
        .is_closed());

    // A real resolution closes it; deleting that resolution reopens it.
    let real = resolution(&keys, channel, page, &s1, "rejected", 1_200);
    ingest(&db, community, &real).await;
    assert_eq!(
        state_of(&db, community, channel, page, &s1).await,
        SuggestionState {
            resolved: true,
            applied: false
        }
    );
    crate::event::soft_delete_event(&db.pool, community, real.id.as_bytes())
        .await
        .expect("delete resolution");
    assert!(!state_of(&db, community, channel, page, &s1)
        .await
        .is_closed());

    // An applying revision closes it too, without any resolution.
    let applied = applying_revision(&keys, channel, page, &r1, &s1, 1_300);
    ingest(&db, community, &applied).await;
    assert_eq!(
        state_of(&db, community, channel, page, &s1).await,
        SuggestionState {
            resolved: false,
            applied: true
        }
    );
}

// -- Reads, lock, deletion -----------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn referenced_events_load_per_community_and_hide_deleted_events() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let other_community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    ingest(&db, community, &r1).await;

    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let loaded = load_event_in_transaction(&mut tx, community, &id32(&r1))
        .await
        .expect("load")
        .expect("revision is live");
    assert_eq!(loaded.kind, KIND_PAGE_REVISION);
    assert_eq!(loaded.channel_id, Some(channel));
    assert_eq!(loaded.page_identity(), Some((channel, page)));
    assert_eq!(loaded.tag_value("title"), Some("v1"));
    assert_eq!(loaded.content, "body of v1");
    assert_eq!(loaded.author, keys.public_key().to_bytes());

    assert!(
        load_event_in_transaction(&mut tx, other_community, &id32(&r1))
            .await
            .expect("load other community")
            .is_none(),
        "another community must not see the event"
    );
    drop(tx);

    crate::event::soft_delete_event(&db.pool, community, r1.id.as_bytes())
        .await
        .expect("delete");
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    assert!(load_event_in_transaction(&mut tx, community, &id32(&r1))
        .await
        .expect("load deleted")
        .is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn insert_page_event_rejects_other_kinds_and_reports_duplicates() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let note = page_event(&keys, 9, vec![], "chat", 1_000);
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    assert!(matches!(
        insert_page_event_in_transaction(&mut tx, community, &note, channel).await,
        Err(DbError::InvalidData(_))
    ));
    drop(tx);

    let r1 = revision(&keys, channel, Uuid::new_v4(), None, "v1", 1_000);
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let (_, first) = insert_page_event_in_transaction(&mut tx, community, &r1, channel)
        .await
        .expect("insert");
    let (_, again) = insert_page_event_in_transaction(&mut tx, community, &r1, channel)
        .await
        .expect("duplicate insert");
    assert!(
        first && !again,
        "second insert of the same id is a duplicate"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn page_writer_lock_serializes_one_page_and_times_out_instead_of_hanging() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();

    let mut holder = db.begin_event_write_transaction().await.expect("tx");
    lock_page_for_write_in_transaction(&mut holder, community, channel, page)
        .await
        .expect("first writer locks the page");

    // A writer of ANOTHER page is not blocked.
    let mut elsewhere = db.begin_event_write_transaction().await.expect("tx");
    lock_page_for_write_in_transaction(&mut elsewhere, community, channel, Uuid::new_v4())
        .await
        .expect("a different page does not wait");
    drop(elsewhere);

    // A second writer of the same page waits, then fails with a lock timeout the
    // relay maps to a retryable conflict.
    let mut second = db.begin_event_write_transaction().await.expect("tx");
    let started = Instant::now();
    let error = lock_page_for_write_in_transaction(&mut second, community, channel, page)
        .await
        .expect_err("the page is locked");
    let waited = started.elapsed();
    assert!(is_lock_timeout(&error), "unexpected error: {error:?}");
    assert!(
        waited >= Duration::from_millis(u64::from(PAGE_WRITE_LOCK_TIMEOUT_MS) - 500)
            && waited < Duration::from_millis(u64::from(PAGE_WRITE_LOCK_TIMEOUT_MS) * 3),
        "waited {waited:?}"
    );
    drop(second);

    // Once the holder finishes the lock is free again.
    holder.commit().await.expect("commit");
    let mut third = db.begin_event_write_transaction().await.expect("tx");
    lock_page_for_write_in_transaction(&mut third, community, channel, page)
        .await
        .expect("lock is free after commit");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn deleting_a_revision_repairs_the_head_in_the_same_transaction() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    let r2 = revision(&keys, channel, page, Some(&r1), "v2", 2_000);
    let s1 = suggestion(&keys, channel, page, &r2, 2_100);
    for event in [&r1, &r2, &s1] {
        ingest(&db, community, event).await;
    }

    // A suggestion deletes without touching the head.
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let outcome = soft_delete_page_event_in_transaction(&mut tx, community, s1.id.as_bytes())
        .await
        .expect("delete suggestion");
    tx.commit().await.expect("commit");
    assert_eq!(outcome, PageEventDeletion::Deleted { page: None });
    assert!(!event_is_live(&db, community, &s1).await);
    let row = get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(row.head_event_id, id32(&r2));

    // Deleting the head falls back to its prev, and a rebuild agrees.
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let outcome = soft_delete_page_event_in_transaction(&mut tx, community, r2.id.as_bytes())
        .await
        .expect("delete head");
    tx.commit().await.expect("commit");
    let PageEventDeletion::Deleted {
        page: Some(repaired),
    } = outcome
    else {
        panic!("deleting a revision must report the repaired row, got {outcome:?}");
    };
    assert_eq!(repaired.head_event_id, id32(&r1));
    assert_eq!(repaired.title, "v1");
    let live = live_rows(&db, community).await;
    assert_eq!(live, vec![repaired.clone()]);
    db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(live_rows(&db, community).await, live);

    // A later edit builds on the repaired head; one built on the deleted
    // revision conflicts.
    let r3 = revision(&keys, channel, page, Some(&r1), "v3", 3_000);
    ingest(&db, community, &r3).await;
    let stale = revision(&keys, channel, page, Some(&r2), "stale", 3_100);
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    insert_page_event_in_transaction(&mut tx, community, &stale, channel)
        .await
        .expect("insert");
    assert_eq!(
        conflict_head(record_page_revision_in_transaction(&mut tx, community, &meta(&stale)).await),
        Some(id32(&r3))
    );
    drop(tx);

    // Deleting the last live revisions removes the row.
    for event in [&r3, &r1] {
        let mut tx = db.begin_event_write_transaction().await.expect("tx");
        soft_delete_page_event_in_transaction(&mut tx, community, event.id.as_bytes())
            .await
            .expect("delete");
        tx.commit().await.expect("commit");
    }
    assert!(live_rows(&db, community).await.is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn deleting_reports_non_page_and_missing_and_already_deleted_events() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let note = page_event(&keys, 9, vec![pair("h", channel)], "chat", 1_000);
    crate::event::insert_event(&db.pool, community, &note, Some(channel))
        .await
        .expect("store note");

    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    assert_eq!(
        soft_delete_page_event_in_transaction(&mut tx, community, note.id.as_bytes())
            .await
            .expect("note"),
        PageEventDeletion::NotAPageEvent
    );
    assert!(
        event_is_live(&db, community, &note).await,
        "a non-page event is left for the generic delete path"
    );
    assert_eq!(
        soft_delete_page_event_in_transaction(&mut tx, community, &[9_u8; 32])
            .await
            .expect("missing"),
        PageEventDeletion::NotFound
    );
    drop(tx);

    let page = Uuid::new_v4();
    let r1 = revision(&keys, channel, page, None, "v1", 2_000);
    ingest(&db, community, &r1).await;
    for expected_deleted in [true, false] {
        let mut tx = db.begin_event_write_transaction().await.expect("tx");
        let outcome = soft_delete_page_event_in_transaction(&mut tx, community, r1.id.as_bytes())
            .await
            .expect("delete");
        tx.commit().await.expect("commit");
        assert_eq!(
            matches!(outcome, PageEventDeletion::Deleted { .. }),
            expected_deleted
        );
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_transaction_that_dies_after_event_and_head_leaves_neither() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    ingest(&db, community, &r1).await;
    let r2 = revision(&keys, channel, page, Some(&r1), "v2", 2_000);

    // Both writes succeed inside the transaction, then it is abandoned before
    // commit (a crash, a lost connection, a failed commit).
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    insert_page_event_in_transaction(&mut tx, community, &r2, channel)
        .await
        .expect("insert");
    record_page_revision_in_transaction(&mut tx, community, &meta(&r2))
        .await
        .expect("move head");
    drop(tx);

    assert!(!event_is_live(&db, community, &r2).await);
    let row = get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(row.head_event_id, id32(&r1), "head must not have advanced");
    assert_eq!(row.revision_count, 1);
}
