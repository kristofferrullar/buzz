//! Postgres-backed tests of the page search projection (`pages.search_tsv`,
//! fork-private migration 9002).
//!
//! The projection is maintained by a trigger on the head columns, so these tests
//! drive every production writer of `pages` (create, advance, delete-repair by
//! re-projection, rebuild by replay) and check that the vector always names the
//! current head and nothing else.

use super::*;

/// The page's search vector as text (`None` when NULL).
async fn search_vector(
    db: &Db,
    community: CommunityId,
    channel: Uuid,
    page: Uuid,
) -> Option<String> {
    sqlx::query_scalar(
        "SELECT search_tsv::text FROM pages \
         WHERE community_id = $1 AND channel_id = $2 AND page_id = $3",
    )
    .bind(community.as_uuid())
    .bind(channel)
    .bind(page)
    .fetch_one(&db.pool)
    .await
    .expect("read search vector")
}

fn names(vector: &Option<String>, word: &str) -> bool {
    vector
        .as_deref()
        .is_some_and(|text| text.contains(&format!("'{word}'")))
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn search_vector_follows_the_head_through_create_advance_repair_and_rebuild() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();

    // Create: the first revision's title and content are projected.
    let r1 = revision(&keys, channel, page, None, "alphatitle", 1_000);
    publish(&db, community, &r1).await.expect("create");
    let vector = search_vector(&db, community, channel, page).await;
    assert!(names(&vector, "alphatitle"), "{vector:?}");
    assert!(names(&vector, "body"), "content is projected: {vector:?}");

    // Advance: the new head replaces the old one in the vector.
    let r2 = revision(&keys, channel, page, Some(&r1), "betatitle", 2_000);
    publish(&db, community, &r2).await.expect("advance");
    let vector = search_vector(&db, community, channel, page).await;
    assert!(names(&vector, "betatitle"), "{vector:?}");
    assert!(
        !names(&vector, "alphatitle"),
        "a superseded revision must not stay searchable: {vector:?}"
    );

    // A stored revision that is not the head (an import, a fork) changes nothing.
    let side = revision(&keys, channel, page, Some(&r1), "sidetitle", 2_500);
    store_only(&db, community, &side).await;
    let vector = search_vector(&db, community, channel, page).await;
    assert!(names(&vector, "betatitle") && !names(&vector, "sidetitle"));

    // Delete-repair: soft-deleting the head and re-projecting falls back to its
    // prev, and the vector follows.
    crate::event::soft_delete_event(&db.pool, community, r2.id.as_bytes())
        .await
        .expect("delete head event");
    let mut tx = db.begin_event_write_transaction().await.expect("tx");
    reproject_page_in_transaction(&mut tx, community, channel, page)
        .await
        .expect("reproject")
        .expect("page still has live revisions");
    tx.commit().await.expect("commit");
    let repaired = search_vector(&db, community, channel, page).await;
    assert!(names(&repaired, "alphatitle"), "{repaired:?}");
    assert!(
        !names(&repaired, "betatitle"),
        "a deleted head must not stay searchable: {repaired:?}"
    );

    // Rebuild by replay reproduces the vector exactly (NIP-PG rebuild
    // invariant: the projection is derived data).
    wipe_index(&db, community).await;
    db.rebuild_pages(community).await.expect("rebuild");
    assert_eq!(
        search_vector(&db, community, channel, page).await,
        repaired,
        "the rebuilt vector must equal the live one"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_rolled_back_head_move_leaves_the_search_vector_unchanged() {
    let db = test_db().await;
    let community = make_community(&db).await;
    let channel = make_channel(&db, community).await;
    let keys = Keys::generate();
    let page = Uuid::new_v4();
    let r1 = revision(&keys, channel, page, None, "stabletitle", 1_000);
    publish(&db, community, &r1).await.expect("create");
    let before = search_vector(&db, community, channel, page).await;

    // A stale `prev` conflicts: the event insert and the head swap roll back
    // together, so the vector cannot drift ahead of the head.
    let stale = revision(&keys, channel, page, Some(&r1), "rejectedtitle", 2_000);
    let winner = revision(&keys, channel, page, Some(&r1), "winnertitle", 2_100);
    publish(&db, community, &winner).await.expect("winner");
    assert!(publish(&db, community, &stale).await.is_err());
    let after = search_vector(&db, community, channel, page).await;
    assert!(names(&after, "winnertitle"), "{after:?}");
    assert!(!names(&after, "rejectedtitle"), "{after:?}");
    assert_ne!(before, after);
}
