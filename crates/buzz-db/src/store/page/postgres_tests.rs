//! Postgres-backed tests of the pages head index.
//!
//! Every test drives the production seam: events are stored with
//! `insert_event_in_transaction` and the head moved with
//! `record_page_revision_in_transaction` inside one transaction from
//! `Db::begin_event_write_transaction`, exactly as the relay composes them.

use std::time::Duration;

use nostr::{Event, EventBuilder, Keys, Kind, Tag, Timestamp};

use super::*;
use crate::store::deletion::{
    FrozenInventory, KeyStreamDigest, LeaseToken, PrefixManifest, StorageManifest,
    DEFAULT_LEASE_DURATION, EXPECTED_SCOPED_TABLES, PURGE_SCOPED_TABLES,
};

mod ingest;

// -- Fixtures ------------------------------------------------------------------

async fn test_db() -> Db {
    let pool = PgPool::connect(&crate::test_support::database_url())
        .await
        .expect("connect to test DB");
    if std::env::var("BUZZ_TEST_SCHEMA_MODE").as_deref() == Ok("migration") {
        crate::migration::run_migrations(&pool)
            .await
            .expect("apply migration schema");
    }
    Db::from_pool(pool)
}

async fn make_community(db: &Db) -> CommunityId {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
        .bind(id)
        .bind(format!("pages-test-{}.example", id.simple()))
        .execute(&db.pool)
        .await
        .expect("insert community");
    CommunityId::from_uuid(id)
}

/// Insert a channel with the given id (or a fresh one) into `community`.
async fn make_channel_with_id(db: &Db, community: CommunityId, id: Option<Uuid>) -> Uuid {
    let id = id.unwrap_or_else(Uuid::new_v4);
    sqlx::query(
        "INSERT INTO channels (id, community_id, name, created_by) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(community.as_uuid())
    .bind(format!("pages-{}", id.simple()))
    .bind(vec![1_u8; 32])
    .execute(&db.pool)
    .await
    .expect("insert channel");
    id
}

async fn make_channel(db: &Db, community: CommunityId) -> Uuid {
    make_channel_with_id(db, community, None).await
}

/// A signed `PAGE_REVISION`. `prev` is the revision this one is based on.
fn revision(
    keys: &Keys,
    channel: Uuid,
    page: Uuid,
    prev: Option<&Event>,
    title: &str,
    secs: u64,
) -> Event {
    let mut tags = vec![
        Tag::parse(["h", &channel.to_string()]).expect("h tag"),
        Tag::parse(["d", &page.to_string()]).expect("d tag"),
        Tag::parse(["title", title]).expect("title tag"),
    ];
    if let Some(prev) = prev {
        tags.push(Tag::parse(["prev", &prev.id.to_hex()]).expect("prev tag"));
    }
    EventBuilder::new(
        Kind::Custom(KIND_PAGE_REVISION as u16),
        format!("body of {title}"),
    )
    .tags(tags)
    .custom_created_at(Timestamp::from(secs))
    .sign_with_keys(keys)
    .expect("sign revision")
}

fn meta(event: &Event) -> PageRevisionMeta {
    PageRevisionMeta::from_event(event).expect("valid page revision")
}

/// Store `event` and move the page head in one transaction, as the relay does.
/// Any error drops the transaction, rolling back both writes.
async fn publish(
    db: &Db,
    community: CommunityId,
    event: &Event,
) -> std::result::Result<PageRecord, PageHeadError> {
    let revision = meta(event);
    let mut tx = db.begin_event_write_transaction().await?;
    crate::event::insert_event_in_transaction(&mut tx, community, event, Some(revision.channel_id))
        .await?;
    let record = record_page_revision_in_transaction(&mut tx, community, &revision).await?;
    tx.commit().await.map_err(DbError::from)?;
    Ok(record)
}

/// Store an event without touching the index (an import, or a fork).
async fn store_only(db: &Db, community: CommunityId, event: &Event) {
    let channel = meta(event).channel_id;
    crate::event::insert_event(&db.pool, community, event, Some(channel))
        .await
        .expect("store event");
}

async fn event_is_live(db: &Db, community: CommunityId, event: &Event) -> bool {
    crate::event::get_event_by_id(&db.pool, community, event.id.as_bytes())
        .await
        .expect("read event")
        .is_some()
}

/// Every live row of the community's index, in a stable order.
async fn live_rows(db: &Db, community: CommunityId) -> Vec<PageRecord> {
    sqlx::query(concat!(
        "SELECT ",
        page_columns!(),
        " FROM pages WHERE community_id = $1 AND deleted_at IS NULL \
         ORDER BY channel_id, page_id"
    ))
    .bind(community.as_uuid())
    .fetch_all(&db.pool)
    .await
    .expect("read live pages")
    .iter()
    .map(|row| row_to_record(row).expect("page row"))
    .collect()
}

async fn wipe_index(db: &Db, community: CommunityId) {
    sqlx::query("DELETE FROM pages WHERE community_id = $1")
        .bind(community.as_uuid())
        .execute(&db.pool)
        .await
        .expect("wipe page index");
}

fn conflict_head(result: std::result::Result<PageRecord, PageHeadError>) -> Option<[u8; 32]> {
    match result {
        Err(PageHeadError::Conflict { current_head }) => current_head,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

fn id32(event: &Event) -> [u8; 32] {
    *event.id.as_bytes()
}

// -- Head create and CAS -------------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn first_revision_creates_the_page_and_a_second_create_conflicts() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();

    let r1 = revision(&keys, channel, page, None, "Plan", 1_000);
    let record = publish(&db, community, &r1).await.expect("create page");
    assert_eq!(record.head_event_id, id32(&r1));
    assert_eq!(record.title, "Plan");
    assert_eq!(record.created_by, keys.public_key().to_bytes());
    assert_eq!(record.updated_by, record.created_by);
    assert_eq!(record.created_at.timestamp(), 1_000);
    assert_eq!(record.updated_at, record.created_at);
    assert_eq!(record.revision_count, 1);
    assert_eq!(record.deleted_at, None);

    // Another first revision for the same identity names a page that exists.
    let again = revision(&keys, channel, page, None, "Other", 1_001);
    assert_eq!(
        conflict_head(publish(&db, community, &again).await),
        Some(id32(&r1)),
        "a second no-prev revision must conflict and carry the current head"
    );
    assert!(
        !event_is_live(&db, community, &again).await,
        "the conflicting revision's event must not be stored"
    );
    let stored = get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(stored.head_event_id, id32(&r1));
    assert_eq!(stored.revision_count, 1);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn later_revision_requires_prev_to_equal_the_current_head() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let alice = Keys::generate();
    let bob = Keys::generate();

    let r1 = revision(&alice, channel, page, None, "v1", 1_000);
    publish(&db, community, &r1).await.expect("r1");
    let r2 = revision(&bob, channel, page, Some(&r1), "v2", 2_000);
    let after_r2 = publish(&db, community, &r2).await.expect("r2 advances");
    assert_eq!(after_r2.head_event_id, id32(&r2));
    assert_eq!(after_r2.title, "v2");
    assert_eq!(after_r2.updated_by, bob.public_key().to_bytes());
    assert_eq!(after_r2.created_by, alice.public_key().to_bytes());
    assert_eq!(after_r2.updated_at.timestamp(), 2_000);
    assert_eq!(after_r2.created_at.timestamp(), 1_000);
    assert_eq!(after_r2.revision_count, 2);

    // A revision based on the superseded r1 is stale.
    let stale = revision(&alice, channel, page, Some(&r1), "stale", 3_000);
    assert_eq!(
        conflict_head(publish(&db, community, &stale).await),
        Some(id32(&r2))
    );
    assert!(!event_is_live(&db, community, &stale).await);

    // A revision based on an event that was never the head is stale too.
    let stranger = revision(&alice, channel, page, None, "elsewhere", 500);
    let on_stranger = revision(&alice, channel, page, Some(&stranger), "dangling", 3_500);
    assert_eq!(
        conflict_head(publish(&db, community, &on_stranger).await),
        Some(id32(&r2))
    );

    let after = get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(
        after.head_event_id,
        id32(&r2),
        "head must not move on conflict"
    );
    assert_eq!(after.revision_count, 2);

    // The right prev still advances afterwards.
    let r3 = revision(&alice, channel, page, Some(&r2), "v3", 4_000);
    assert_eq!(
        publish(&db, community, &r3)
            .await
            .expect("r3")
            .revision_count,
        3
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn later_revision_for_a_missing_page_conflicts_with_no_head() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let phantom_prev = revision(&keys, channel, page, None, "never stored", 1_000);
    let orphan = revision(&keys, channel, page, Some(&phantom_prev), "orphan", 2_000);
    assert_eq!(conflict_head(publish(&db, community, &orphan).await), None);
    assert!(get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn create_and_advance_reject_the_wrong_kind_of_revision() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let first = revision(&keys, channel, page, None, "v1", 1_000);
    let second = revision(&keys, channel, page, Some(&first), "v2", 2_000);

    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let create_with_prev =
        create_page_head_in_transaction(&mut tx, community, &meta(&second)).await;
    assert!(matches!(
        create_with_prev,
        Err(PageHeadError::Db(DbError::InvalidData(_)))
    ));
    let advance_without_prev =
        advance_page_head_in_transaction(&mut tx, community, &meta(&first)).await;
    assert!(matches!(
        advance_without_prev,
        Err(PageHeadError::Db(DbError::InvalidData(_)))
    ));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn failed_head_swap_rolls_back_the_event_insert_in_the_same_transaction() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    let r2 = revision(&keys, channel, page, Some(&r1), "v2", 2_000);
    publish(&db, community, &r1).await.expect("r1");
    publish(&db, community, &r2).await.expect("r2");

    // A writer that loses the swap: its event insert and swap share one
    // transaction, so the caller rolling back leaves no trace of the event.
    let stale = revision(&keys, channel, page, Some(&r1), "stale", 3_000);
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    crate::event::insert_event_in_transaction(&mut tx, community, &stale, Some(channel))
        .await
        .expect("insert event");
    let outcome = record_page_revision_in_transaction(&mut tx, community, &meta(&stale)).await;
    assert_eq!(conflict_head(outcome), Some(id32(&r2)));
    tx.rollback().await.expect("rollback");
    assert!(!event_is_live(&db, community, &stale).await);

    // And a winner's event and head commit together.
    let r3 = revision(&keys, channel, page, Some(&r2), "v3", 4_000);
    publish(&db, community, &r3).await.expect("r3");
    assert!(event_is_live(&db, community, &r3).await);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn racing_writers_serialize_on_the_head_and_the_loser_gets_a_conflict() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    publish(&db, community, &r1).await.expect("r1");

    let winner = revision(&keys, channel, page, Some(&r1), "winner", 2_000);
    let loser = revision(&keys, channel, page, Some(&r1), "loser", 2_001);

    // The first writer takes the head and holds its transaction open.
    let mut tx_first = db.begin_event_write_transaction().await.expect("tx");
    crate::event::insert_event_in_transaction(&mut tx_first, community, &winner, Some(channel))
        .await
        .expect("insert winner");
    record_page_revision_in_transaction(&mut tx_first, community, &meta(&winner))
        .await
        .expect("winner swaps");

    // The second writer races on the same head from another connection. It must
    // block on the row lock rather than slip past, then lose once re-evaluated.
    let racer = {
        let db = db.clone();
        let loser = loser.clone();
        tokio::spawn(async move { publish(&db, community, &loser).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !racer.is_finished(),
        "a second swap on the same head must wait for the first transaction"
    );
    tx_first.commit().await.expect("commit winner");
    let outcome = tokio::time::timeout(Duration::from_secs(10), racer)
        .await
        .expect("racer finishes after the winner commits")
        .expect("racer task");
    assert_eq!(conflict_head(outcome), Some(id32(&winner)));
    assert!(!event_is_live(&db, community, &loser).await);
    let page_row = get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(page_row.head_event_id, id32(&winner));
    assert_eq!(page_row.revision_count, 2);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn simultaneous_advances_from_two_tasks_have_exactly_one_winner() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();

    for round in 0..12_u64 {
        let page = Uuid::new_v4();
        let r1 = revision(&keys, channel, page, None, "v1", 1_000);
        publish(&db, community, &r1).await.expect("r1");
        let a = revision(&keys, channel, page, Some(&r1), "a", 2_000 + round);
        let b = revision(&keys, channel, page, Some(&r1), "b", 3_000 + round);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let spawn_writer = |event: Event| {
            let db = db.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                publish(&db, community, &event).await
            })
        };
        let (first, second) = tokio::join!(spawn_writer(a.clone()), spawn_writer(b.clone()));
        let results = [first.expect("task a"), second.expect("task b")];
        let winners = results.iter().filter(|r| r.is_ok()).count();
        let conflicts = results
            .iter()
            .filter(|r| matches!(r, Err(PageHeadError::Conflict { .. })))
            .count();
        assert_eq!((winners, conflicts), (1, 1), "round {round}: {results:?}");
        let record = get_page(&db.pool, community, channel, page)
            .await
            .expect("get")
            .expect("page");
        assert_eq!(record.revision_count, 2, "round {round}");
        let live: Vec<bool> = vec![
            event_is_live(&db, community, &a).await,
            event_is_live(&db, community, &b).await,
        ];
        assert_eq!(
            live.iter().filter(|stored| **stored).count(),
            1,
            "round {round}: exactly the winner's event is stored"
        );
    }
}

// -- Identity and isolation ----------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn same_page_id_in_two_channels_is_two_independent_pages() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel_a = make_channel(&db, community).await;
    let channel_b = make_channel(&db, community).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();

    let a1 = revision(&keys, channel_a, page, None, "in A", 1_000);
    let b1 = revision(&keys, channel_b, page, None, "in B", 1_100);
    publish(&db, community, &a1).await.expect("create in A");
    publish(&db, community, &b1)
        .await
        .expect("the same d in another channel is a new page, not a conflict");

    let a2 = revision(&keys, channel_a, page, Some(&a1), "A v2", 2_000);
    publish(&db, community, &a2).await.expect("advance A");

    let in_a = get_page(&db.pool, community, channel_a, page)
        .await
        .expect("get A")
        .expect("page A");
    let in_b = get_page(&db.pool, community, channel_b, page)
        .await
        .expect("get B")
        .expect("page B");
    assert_eq!(in_a.head_event_id, id32(&a2));
    assert_eq!(in_a.revision_count, 2);
    assert_eq!(
        in_b.head_event_id,
        id32(&b1),
        "advancing A must not touch B"
    );
    assert_eq!(in_b.revision_count, 1);
    assert_eq!(in_b.title, "in B");

    // B's revision cannot be based on A's chain: prev is checked per (h, d).
    let cross = revision(&keys, channel_b, page, Some(&a2), "cross", 3_000);
    assert_eq!(
        conflict_head(publish(&db, community, &cross).await),
        Some(id32(&b1))
    );

    let listing = list_pages_for_channels(
        &db.pool,
        community,
        &[channel_a, channel_b],
        PAGE_LIST_DEFAULT_LIMIT,
        None,
    )
    .await
    .expect("list");
    assert_eq!(listing.pages.len(), 2, "two pages share one page id");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn pages_are_invisible_across_communities_even_with_colliding_ids() {
    let db = test_db().await;
    let community_a = make_community(&db).await;
    let community_b = make_community(&db).await;
    // The same channel UUID legitimately exists in two communities.
    let shared_channel = Uuid::new_v4();
    make_channel_with_id(&db, community_a, Some(shared_channel)).await;
    make_channel_with_id(&db, community_b, Some(shared_channel)).await;
    let a_only_channel = make_channel(&db, community_a).await;
    let page = Uuid::new_v4();
    let keys = Keys::generate();

    let a1 = revision(&keys, shared_channel, page, None, "tenant A", 1_000);
    publish(&db, community_a, &a1).await.expect("create in A");

    // B sees nothing of A's page, by identity or by listing.
    assert!(get_page(&db.pool, community_b, shared_channel, page)
        .await
        .expect("get")
        .is_none());
    assert!(
        get_page_including_deleted(&db.pool, community_b, shared_channel, page)
            .await
            .expect("get")
            .is_none()
    );
    let b_listing = list_pages_for_channels(
        &db.pool,
        community_b,
        &[shared_channel, a_only_channel],
        PAGE_LIST_MAX_LIMIT,
        None,
    )
    .await
    .expect("list B");
    assert!(b_listing.pages.is_empty(), "B must not list A's pages");

    // B can create a page with the very same (h, d); it is a different page.
    let b1 = revision(&keys, shared_channel, page, None, "tenant B", 1_500);
    publish(&db, community_b, &b1)
        .await
        .expect("same (h, d) in another community");
    assert_eq!(
        get_page(&db.pool, community_a, shared_channel, page)
            .await
            .expect("get A")
            .expect("page A")
            .title,
        "tenant A"
    );
    assert_eq!(
        get_page(&db.pool, community_b, shared_channel, page)
            .await
            .expect("get B")
            .expect("page B")
            .title,
        "tenant B"
    );

    // A head swap in B cannot be satisfied with A's head, and vice versa.
    let b_on_a_head = revision(&keys, shared_channel, page, Some(&a1), "bad", 2_000);
    assert_eq!(
        conflict_head(publish(&db, community_b, &b_on_a_head).await),
        Some(id32(&b1))
    );

    // Deleting in A leaves B's page and events alone.
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    assert!(
        soft_delete_page_in_transaction(&mut tx, community_a, shared_channel, page)
            .await
            .expect("delete in A")
    );
    tx.commit().await.expect("commit");
    assert!(get_page(&db.pool, community_a, shared_channel, page)
        .await
        .expect("get")
        .is_none());
    assert!(get_page(&db.pool, community_b, shared_channel, page)
        .await
        .expect("get")
        .is_some());
    assert!(event_is_live(&db, community_b, &b1).await);

    // Listing A with B-only knowledge: channel ids of another community match nothing.
    let a_listing = list_pages_for_channels(
        &db.pool,
        community_a,
        &[shared_channel, a_only_channel],
        PAGE_LIST_MAX_LIMIT,
        None,
    )
    .await
    .expect("list A");
    assert!(a_listing.pages.is_empty());
}

// -- Listing -------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn listing_is_newest_updated_first_and_pages_without_gaps_or_repeats() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channels = [
        make_channel(&db, community).await,
        make_channel(&db, community).await,
        make_channel(&db, community).await,
    ];
    let keys = Keys::generate();
    // Several pages share an updated_at second, so the (channel, page) tiebreak
    // is exercised across batch boundaries.
    let stamps = [500_u64, 900, 900, 900, 700, 700, 1_200, 100];
    for (n, secs) in stamps.iter().enumerate() {
        let channel = channels[n % channels.len()];
        let r1 = revision(
            &keys,
            channel,
            Uuid::new_v4(),
            None,
            &format!("p{n}"),
            *secs,
        );
        publish(&db, community, &r1).await.expect("create page");
    }
    let mut expected = live_rows(&db, community).await;
    expected.sort_by_key(|p| std::cmp::Reverse((p.updated_at, p.channel_id, p.page_id)));
    assert_eq!(expected.len(), stamps.len());
    assert_eq!(expected[0].updated_at.timestamp(), 1_200);
    assert_eq!(expected.last().expect("last").updated_at.timestamp(), 100);

    for limit in [1_u32, 2, 3, 7, 8, 100] {
        let mut collected = Vec::new();
        let mut cursor: Option<PageCursor> = None;
        let mut batches = 0;
        loop {
            let batch =
                list_pages_for_channels(&db.pool, community, &channels, limit, cursor.as_ref())
                    .await
                    .expect("list batch");
            batches += 1;
            assert!(batch.pages.len() <= limit as usize);
            collected.extend(batch.pages);
            match batch.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
            assert!(
                batches <= stamps.len() + 1,
                "limit {limit}: runaway pagination"
            );
        }
        assert_eq!(collected, expected, "limit {limit}");
    }

    // A full page is not followed by a phantom empty batch.
    let exact = list_pages_for_channels(&db.pool, community, &channels, 8, None)
        .await
        .expect("list");
    assert_eq!(exact.pages.len(), 8);
    assert_eq!(exact.next_cursor, None);
    // limit 0 is clamped up, not an error and not unbounded.
    let clamped = list_pages_for_channels(&db.pool, community, &channels, 0, None)
        .await
        .expect("list");
    assert_eq!(clamped.pages.len(), 1);
    // Only the requested channels are listed.
    let one = list_pages_for_channels(&db.pool, community, &channels[..1], 100, None)
        .await
        .expect("list");
    assert!(one.pages.iter().all(|p| p.channel_id == channels[0]));
    assert_eq!(one.pages.len(), 3);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn listing_a_large_channel_set_is_chunked_and_still_ordered() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let keys = Keys::generate();
    let mut real = Vec::new();
    for secs in [300_u64, 100, 200] {
        let channel = make_channel(&db, community).await;
        let r1 = revision(&keys, channel, Uuid::new_v4(), None, "p", secs);
        publish(&db, community, &r1).await.expect("create page");
        real.push((channel, secs));
    }
    // 2,500 channel ids in total with the real ones landing in different chunks.
    let mut ids: Vec<Uuid> = (0..2_500).map(|_| Uuid::new_v4()).collect();
    ids[0] = real[1].0;
    ids[1_400] = real[2].0;
    ids[2_499] = real[0].0;
    let listing = list_pages_for_channels(&db.pool, community, &ids, 10, None)
        .await
        .expect("list");
    let secs: Vec<i64> = listing
        .pages
        .iter()
        .map(|p| p.updated_at.timestamp())
        .collect();
    assert_eq!(secs, vec![300, 200, 100]);
    assert_eq!(listing.next_cursor, None);

    let first_only = list_pages_for_channels(&db.pool, community, &ids, 1, None)
        .await
        .expect("list");
    assert_eq!(first_only.pages.len(), 1);
    assert_eq!(first_only.pages[0].updated_at.timestamp(), 300);
    assert!(first_only.next_cursor.is_some());
}

// -- Soft delete ---------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn soft_deleted_page_is_hidden_blocked_and_takes_its_events_with_it() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let doomed = Uuid::new_v4();
    let survivor = Uuid::new_v4();

    let d1 = revision(&keys, channel, doomed, None, "doomed v1", 1_000);
    let d2 = revision(&keys, channel, doomed, Some(&d1), "doomed v2", 2_000);
    let s1 = revision(&keys, channel, survivor, None, "survivor", 3_000);
    for event in [&d1, &d2, &s1] {
        publish(&db, community, event).await.expect("publish");
    }

    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    assert!(
        soft_delete_page_in_transaction(&mut tx, community, channel, doomed)
            .await
            .expect("delete")
    );
    tx.commit().await.expect("commit");

    assert!(get_page(&db.pool, community, channel, doomed)
        .await
        .expect("get")
        .is_none());
    let tombstone = get_page_including_deleted(&db.pool, community, channel, doomed)
        .await
        .expect("get")
        .expect("tombstone");
    assert!(tombstone.deleted_at.is_some());
    let listing = list_pages_for_channels(&db.pool, community, &[channel], 100, None)
        .await
        .expect("list");
    assert_eq!(
        listing.pages.iter().map(|p| p.page_id).collect::<Vec<_>>(),
        vec![survivor],
        "a soft-deleted page is excluded from the library"
    );
    assert!(!event_is_live(&db, community, &d1).await);
    assert!(!event_is_live(&db, community, &d2).await);
    assert!(event_is_live(&db, community, &s1).await);

    // A tombstone accepts no revisions and cannot be re-created.
    let d3 = revision(&keys, channel, doomed, Some(&d2), "after delete", 4_000);
    assert!(matches!(
        publish(&db, community, &d3).await,
        Err(PageHeadError::Deleted)
    ));
    let recreate = revision(&keys, channel, doomed, None, "recreate", 4_100);
    assert!(matches!(
        publish(&db, community, &recreate).await,
        Err(PageHeadError::Deleted)
    ));
    assert!(!event_is_live(&db, community, &d3).await);

    // Deleting again, or deleting a page that never existed, is a no-op.
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    assert!(
        !soft_delete_page_in_transaction(&mut tx, community, channel, doomed)
            .await
            .expect("second delete")
    );
    assert!(
        !soft_delete_page_in_transaction(&mut tx, community, channel, Uuid::new_v4())
            .await
            .expect("unknown page")
    );
    tx.commit().await.expect("commit");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn pages_of_a_deleted_channel_are_not_listed() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let live_channel = make_channel(&db, community).await;
    let gone_channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    for channel in [live_channel, gone_channel] {
        let r1 = revision(&keys, channel, Uuid::new_v4(), None, "page", 1_000);
        publish(&db, community, &r1).await.expect("create");
    }
    sqlx::query("UPDATE channels SET deleted_at = NOW() WHERE community_id = $1 AND id = $2")
        .bind(community.as_uuid())
        .bind(gone_channel)
        .execute(&db.pool)
        .await
        .expect("soft-delete channel");
    let listing = list_pages_for_channels(
        &db.pool,
        community,
        &[live_channel, gone_channel],
        100,
        None,
    )
    .await
    .expect("list");
    assert_eq!(listing.pages.len(), 1);
    assert_eq!(listing.pages[0].channel_id, live_channel);
}

// -- Rebuild by replay ---------------------------------------------------------

/// Build a community with several pages across two channels, one of which shares
/// its page id with a page in the other channel, and one deleted page.
async fn populate_varied_index(db: &Db, community: CommunityId) -> (Uuid, Uuid) {
    let channel_a = make_channel(db, community).await;
    let channel_b = make_channel(db, community).await;
    let alice = Keys::generate();
    let bob = Keys::generate();
    let carol = Keys::generate();
    let shared = Uuid::new_v4();

    // A three-revision page with three authors.
    let p1 = revision(&alice, channel_a, shared, None, "p1 v1", 1_000);
    let p1b = revision(&bob, channel_a, shared, Some(&p1), "p1 v2", 2_000);
    let p1c = revision(&carol, channel_a, shared, Some(&p1b), "p1 v3", 3_000);
    // The same page id in another channel.
    let q1 = revision(&bob, channel_b, shared, None, "q1 v1", 1_500);
    let q1b = revision(&alice, channel_b, shared, Some(&q1), "q1 v2", 2_500);
    // A single-revision page.
    let single = revision(&carol, channel_a, Uuid::new_v4(), None, "single", 4_000);
    // A page whose second revision is stamped before its first (clock skew).
    let skew_page = Uuid::new_v4();
    let skew1 = revision(&alice, channel_b, skew_page, None, "skew v1", 5_000);
    let skew2 = revision(&bob, channel_b, skew_page, Some(&skew1), "skew v2", 4_900);
    // A page that is deleted.
    let gone_page = Uuid::new_v4();
    let gone1 = revision(&alice, channel_a, gone_page, None, "gone", 6_000);

    for event in [&p1, &p1b, &p1c, &q1, &q1b, &single, &skew1, &skew2, &gone1] {
        publish(db, community, event).await.expect("publish");
    }
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    assert!(
        soft_delete_page_in_transaction(&mut tx, community, channel_a, gone_page)
            .await
            .expect("delete page")
    );
    tx.commit().await.expect("commit");
    (channel_a, channel_b)
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn rebuilding_from_events_reproduces_the_live_index() {
    let db = test_db().await;
    let community = make_community(&db).await;
    populate_varied_index(&db, community).await;

    let live = live_rows(&db, community).await;
    assert_eq!(
        live.len(),
        4,
        "p1, its twin q1 in the other channel, single and skew"
    );
    assert!(live.iter().any(|p| p.revision_count == 3));

    wipe_index(&db, community).await;
    assert!(live_rows(&db, community).await.is_empty());

    let report = db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(
        report,
        PageRebuildReport {
            pages_upserted: 4,
            pages_removed: 0,
            malformed_events: 0
        }
    );
    assert_eq!(
        live_rows(&db, community).await,
        live,
        "the rebuilt index must equal the live index field for field"
    );

    // Rebuilding a healthy index is a fixed point.
    let again = db.rebuild_pages(community).await.expect("rebuild again");
    assert_eq!(again.pages_upserted, 4);
    assert_eq!(again.pages_removed, 0);
    assert_eq!(live_rows(&db, community).await, live);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn rebuild_repairs_drift_and_drops_rows_without_events() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let (channel_a, _) = populate_varied_index(&db, community).await;
    let live = live_rows(&db, community).await;

    // Drift: a wrong head and title, and a row with no events behind it.
    sqlx::query(
        "UPDATE pages SET head_event_id = $3, title = 'drifted', revision_count = 99 \
         WHERE community_id = $1 AND channel_id = $2",
    )
    .bind(community.as_uuid())
    .bind(channel_a)
    .bind(vec![0xEE_u8; 32])
    .execute(&db.pool)
    .await
    .expect("corrupt rows");
    sqlx::query(
        "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
         created_by, created_at, updated_by, updated_at) \
         VALUES ($1, $2, $3, $4, 'orphan', $5, NOW(), $5, NOW())",
    )
    .bind(community.as_uuid())
    .bind(channel_a)
    .bind(Uuid::new_v4())
    .bind(vec![0xAA_u8; 32])
    .bind(vec![0xBB_u8; 32])
    .execute(&db.pool)
    .await
    .expect("insert orphan row");
    assert_ne!(live_rows(&db, community).await, live);

    let report = db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(
        report.pages_removed, 2,
        "the orphan row and the deleted page's tombstone are removed"
    );
    assert_eq!(live_rows(&db, community).await, live);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn rebuild_of_one_channel_does_not_touch_other_channels_or_communities() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let other_community = make_community(&db).await;
    let (channel_a, channel_b) = populate_varied_index(&db, community).await;
    populate_varied_index(&db, other_community).await;
    let other_live = live_rows(&db, other_community).await;
    let live = live_rows(&db, community).await;

    wipe_index(&db, community).await;
    wipe_index(&db, other_community).await;
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let report = rebuild_pages_for_channel_in_transaction(&mut tx, community, channel_a)
        .await
        .expect("rebuild channel A");
    tx.commit().await.expect("commit");

    let restored = live_rows(&db, community).await;
    assert_eq!(report.pages_upserted, restored.len());
    assert!(!restored.is_empty());
    assert!(
        restored.iter().all(|p| p.channel_id == channel_a),
        "channel B stays untouched by a channel A rebuild"
    );
    assert!(live.iter().any(|p| p.channel_id == channel_b));
    assert!(
        live_rows(&db, other_community).await.is_empty(),
        "another community's index is untouched"
    );

    // Completing the community restores exactly the live set, and only there.
    db.rebuild_pages(community)
        .await
        .expect("rebuild community");
    assert_eq!(live_rows(&db, community).await, live);
    assert!(live_rows(&db, other_community).await.is_empty());
    db.rebuild_pages(other_community)
        .await
        .expect("rebuild other");
    assert_eq!(live_rows(&db, other_community).await, other_live);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn replay_of_a_fork_takes_the_newest_tip_then_the_lowest_event_id() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();

    // Newest tip wins.
    let page = Uuid::new_v4();
    let root = revision(&keys, channel, page, None, "root", 1_000);
    let older_tip = revision(&keys, channel, page, Some(&root), "older tip", 2_000);
    let newer_tip = revision(&keys, channel, page, Some(&root), "newer tip", 3_000);
    for event in [&root, &older_tip, &newer_tip] {
        store_only(&db, community, event).await;
    }

    // Equal created_at: the lowest event id wins.
    let tie_page = Uuid::new_v4();
    let tie_root = revision(&keys, channel, tie_page, None, "tie root", 1_000);
    let tie_a = revision(&keys, channel, tie_page, Some(&tie_root), "tie a", 2_000);
    let tie_b = revision(&keys, channel, tie_page, Some(&tie_root), "tie b", 2_000);
    for event in [&tie_root, &tie_a, &tie_b] {
        store_only(&db, community, event).await;
    }
    let lowest = if id32(&tie_a) < id32(&tie_b) {
        &tie_a
    } else {
        &tie_b
    };

    let report = db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(report.pages_upserted, 2);
    let forked = get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(forked.head_event_id, id32(&newer_tip));
    assert_eq!(forked.title, "newer tip");
    assert_eq!(forked.revision_count, 3);
    assert_eq!(forked.created_by, keys.public_key().to_bytes());
    let tied = get_page(&db.pool, community, channel, tie_page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(tied.head_event_id, id32(lowest));

    // The index is a pure function of the events: rebuilding again changes nothing.
    let before = live_rows(&db, community).await;
    db.rebuild_pages(community).await.expect("rebuild again");
    assert_eq!(live_rows(&db, community).await, before);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn deleted_head_revision_falls_back_to_its_prev_live_and_on_replay() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    let r2 = revision(&keys, channel, page, Some(&r1), "v2", 2_000);
    let r3 = revision(&keys, channel, page, Some(&r2), "v3", 3_000);
    for event in [&r1, &r2, &r3] {
        publish(&db, community, event).await.expect("publish");
    }

    // The head revision's event is deleted; the live index repairs itself from
    // the events by re-projecting the page.
    crate::event::soft_delete_event(&db.pool, community, r3.id.as_bytes())
        .await
        .expect("delete head event");
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let repaired = reproject_page_in_transaction(&mut tx, community, channel, page)
        .await
        .expect("reproject")
        .expect("page still has live revisions");
    tx.commit().await.expect("commit");
    assert_eq!(repaired.head_event_id, id32(&r2));
    assert_eq!(repaired.title, "v2");
    assert_eq!(repaired.revision_count, 3, "deleted revisions still count");

    // A rebuild from scratch agrees with the live repair.
    let live = live_rows(&db, community).await;
    wipe_index(&db, community).await;
    db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(live_rows(&db, community).await, live);

    // Deleting further heads walks further back; no live revision removes the row.
    crate::event::soft_delete_event(&db.pool, community, r2.id.as_bytes())
        .await
        .expect("delete r2 event");
    db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(
        get_page(&db.pool, community, channel, page)
            .await
            .expect("get")
            .expect("page")
            .head_event_id,
        id32(&r1)
    );
    crate::event::soft_delete_event(&db.pool, community, r1.id.as_bytes())
        .await
        .expect("delete r1 event");
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    let gone = reproject_page_in_transaction(&mut tx, community, channel, page)
        .await
        .expect("reproject");
    tx.commit().await.expect("commit");
    assert_eq!(gone, None);
    assert!(
        get_page_including_deleted(&db.pool, community, channel, page)
            .await
            .expect("get")
            .is_none()
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_soft_deleted_page_is_not_resurrected_by_a_rebuild() {
    let db = test_db().await;
    let community = make_community(&db).await;
    populate_varied_index(&db, community).await;
    let live = live_rows(&db, community).await;
    let tombstones: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pages WHERE community_id = $1 AND deleted_at IS NOT NULL",
    )
    .bind(community.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("count tombstones");
    assert_eq!(tombstones, 1);

    let report = db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(
        report.pages_removed, 1,
        "the tombstone has no live revision"
    );
    assert_eq!(live_rows(&db, community).await, live);
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE community_id = $1")
        .bind(community.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("count rows");
    assert_eq!(remaining as usize, live.len());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn replay_skips_and_reports_malformed_page_events() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let good = revision(&keys, channel, page, None, "good", 1_000);
    store_only(&db, community, &good).await;

    // Two titles on one revision, and a page id that is not a UUID.
    let two_titles = EventBuilder::new(Kind::Custom(KIND_PAGE_REVISION as u16), "x")
        .tags([
            Tag::parse(["h", &channel.to_string()]).expect("h"),
            Tag::parse(["d", &Uuid::new_v4().to_string()]).expect("d"),
            Tag::parse(["title", "a"]).expect("title"),
            Tag::parse(["title", "b"]).expect("title"),
        ])
        .sign_with_keys(&keys)
        .expect("sign");
    let bad_page_id = EventBuilder::new(Kind::Custom(KIND_PAGE_REVISION as u16), "x")
        .tags([
            Tag::parse(["h", &channel.to_string()]).expect("h"),
            Tag::parse(["d", "not-a-uuid"]).expect("d"),
            Tag::parse(["title", "t"]).expect("title"),
        ])
        .sign_with_keys(&keys)
        .expect("sign");
    for event in [&two_titles, &bad_page_id] {
        crate::event::insert_event(&db.pool, community, event, Some(channel))
            .await
            .expect("store malformed event");
    }

    let report = db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(report.pages_upserted, 1);
    assert_eq!(report.malformed_events, 2);
    let rows = live_rows(&db, community).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].head_event_id, id32(&good));
}

/// A rebuild computes heads from a snapshot of the events; a writer that is
/// mid-transaction must not be overwritten with that stale snapshot. The rebuild
/// waits for the writer's page lock, then reads the committed events.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn rebuild_waits_for_an_in_flight_writer_instead_of_publishing_a_stale_head() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let r1 = revision(&keys, channel, page, None, "v1", 1_000);
    let r2 = revision(&keys, channel, page, Some(&r1), "v2", 2_000);
    publish(&db, community, &r1).await.expect("r1");

    // The writer has stored r2 and moved the head, but not committed.
    let mut writer = db.begin_event_write_transaction().await.expect("tx");
    crate::event::insert_event_in_transaction(&mut writer, community, &r2, Some(channel))
        .await
        .expect("insert r2");
    record_page_revision_in_transaction(&mut writer, community, &meta(&r2))
        .await
        .expect("advance to r2");

    let rebuild = {
        let db = db.clone();
        tokio::spawn(async move { db.rebuild_pages(community).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !rebuild.is_finished(),
        "a rebuild must wait for in-flight page writers of the channel"
    );
    writer.commit().await.expect("commit r2");
    tokio::time::timeout(Duration::from_secs(10), rebuild)
        .await
        .expect("rebuild finishes once the writer commits")
        .expect("rebuild task")
        .expect("rebuild");

    let record = get_page(&db.pool, community, channel, page)
        .await
        .expect("get")
        .expect("page");
    assert_eq!(
        record.head_event_id,
        id32(&r2),
        "the rebuild must see the writer's committed revision, not overwrite it"
    );
    assert_eq!(record.revision_count, 2);
}

// -- Schema contract -----------------------------------------------------------

#[test]
fn page_table_is_in_the_community_deletion_manifests() {
    assert!(EXPECTED_SCOPED_TABLES.contains(&"pages"));
    let purge_position = |table: &str| PURGE_SCOPED_TABLES.iter().position(|t| *t == table);
    let pages = purge_position("pages").expect("pages is purged");
    let channels = purge_position("channels").expect("channels is purged");
    assert!(
        pages < channels,
        "pages references channels, so it must be purged first"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn pages_table_carries_the_community_write_fence() {
    let db = test_db().await;
    let fenced: Vec<String> = sqlx::query_scalar(
        "SELECT t.tgname FROM pg_trigger t \
         JOIN pg_proc p ON p.oid = t.tgfoid \
         WHERE t.tgrelid = 'pages'::regclass AND NOT t.tgisinternal \
           AND p.proname = 'enforce_community_write_fence' AND t.tgenabled = 'O' \
           AND (t.tgtype & 31) = 31",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read fence triggers");
    assert_eq!(fenced, vec!["community_write_fence_pages".to_owned()]);
}

fn empty_storage_manifest(community: CommunityId) -> StorageManifest {
    let empty = |prefix: String| PrefixManifest {
        prefix,
        object_count: 0,
        total_bytes: 0,
        keys_digest: KeyStreamDigest::new().finish().0,
    };
    StorageManifest {
        version: 4,
        prefixes: vec![
            empty(format!("_meta/{community}/")),
            empty(format!("_uploads/{community}/")),
            empty(format!("repos/{community}/")),
        ],
    }
}

/// A community holding pages can go through the whole-community deletion
/// lifecycle: the catalog check passes with the page table registered, its rows
/// are counted in the frozen inventory, and the purge removes them before the
/// channels they reference.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn whole_community_deletion_purges_page_rows() {
    let db = test_db().await;
    let host = format!("pages-deletion-{}.example", Uuid::new_v4().simple());
    let community = db
        .ensure_configured_community(&host)
        .await
        .expect("create community");
    let community_id = community.id;
    let channel = make_channel(&db, community_id).await;
    let keys = Keys::generate();
    let r1 = revision(&keys, channel, Uuid::new_v4(), None, "doomed page", 1_000);
    publish(&db, community_id, &r1).await.expect("create page");

    let store = db.deletion_store();
    let submitted = store
        .submit(&host, "test-operator", Some("pages deletion"))
        .await
        .expect("submit");
    let inventory = FrozenInventory {
        schema: store
            .inventory_schema(community_id)
            .await
            .expect("schema inventory"),
        storage: empty_storage_manifest(community_id),
    };
    assert_eq!(
        inventory.schema.row_counts.get("pages").copied(),
        Some(1),
        "the frozen inventory counts page rows"
    );
    let request = store
        .freeze_inventory(submitted.id, &inventory)
        .await
        .expect("freeze inventory");
    store
        .approve(request.id, "approver", None)
        .await
        .expect("approve");
    let claim = store
        .claim_specific(request.id, "executor", DEFAULT_LEASE_DURATION)
        .await
        .expect("claim")
        .expect("won claim");
    store.begin_quiescing(&claim.lease).await.expect("quiesce");
    let generation = store.fence(&claim.lease).await.expect("fence");
    let token = LeaseToken {
        fence_generation: Some(generation),
        ..claim.lease
    };
    store
        .freeze_destructive_storage_manifest(&token, &inventory.storage)
        .await
        .expect("freeze destructive storage");
    store.mark_drained(&token).await.expect("drain");
    store
        .mark_bindings_removed(&token, serde_json::json!({"keys": 0}))
        .await
        .expect("bindings");
    let purged = store.purge_postgres(&token).await.expect("purge");
    assert_eq!(purged.get("pages").copied(), Some(1));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE community_id = $1")
        .bind(community_id.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("count pages");
    assert_eq!(left, 0);
}
