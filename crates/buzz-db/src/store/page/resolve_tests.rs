//! The NIP-PG head rules as a pure function. No infrastructure needed.

use super::*;

fn id(n: u8) -> [u8; 32] {
    [n; 32]
}

/// A revision `event` (id byte) authored by `author`, based on `prev`, at second `secs`.
fn node(event: u8, prev: Option<u8>, secs: i64, author: u8, deleted: bool) -> RevisionNode {
    RevisionNode {
        meta: PageRevisionMeta {
            event_id: id(event),
            author: id(author),
            created_at: DateTime::from_timestamp(secs, 0).expect("timestamp"),
            channel_id: Uuid::from_u128(1),
            page_id: Uuid::from_u128(2),
            prev: prev.map(id),
            title: format!("title-{event}"),
        },
        deleted,
    }
}

fn head_of(revisions: &[RevisionNode]) -> Option<[u8; 32]> {
    resolve_page(revisions).map(|page| page.head.event_id)
}

#[test]
fn no_revisions_resolve_to_no_page() {
    assert_eq!(resolve_page(&[]), None);
}

#[test]
fn linear_chain_resolves_to_its_tip_with_root_and_head_metadata() {
    let chain = [
        node(1, None, 100, 10, false),
        node(2, Some(1), 200, 20, false),
        node(3, Some(2), 300, 30, false),
    ];
    let page = resolve_page(&chain).expect("page");
    assert_eq!(page.head.event_id, id(3));
    assert_eq!(page.head.title, "title-3");
    assert_eq!(page.head.author, id(30));
    assert_eq!(page.created_by, id(10), "created_by is the root's author");
    assert_eq!(page.created_at.timestamp(), 100);
    assert_eq!(page.revision_count, 3);
}

#[test]
fn input_order_does_not_change_the_result() {
    let chain = [
        node(3, Some(2), 300, 30, false),
        node(1, None, 100, 10, false),
        node(2, Some(1), 200, 20, false),
    ];
    assert_eq!(head_of(&chain), Some(id(3)));
}

#[test]
fn clock_skew_inside_a_chain_does_not_demote_the_tip() {
    // r2 is stamped earlier than its own prev; it is still the only tip.
    let chain = [
        node(1, None, 500, 10, false),
        node(2, Some(1), 100, 20, false),
    ];
    let page = resolve_page(&chain).expect("page");
    assert_eq!(page.head.event_id, id(2));
    assert_eq!(
        page.created_at.timestamp(),
        500,
        "root timestamp, not the minimum"
    );
}

#[test]
fn fork_resolves_to_the_tip_with_the_greatest_created_at() {
    let fork = [
        node(1, None, 100, 10, false),
        node(2, Some(1), 200, 20, false),
        node(3, Some(1), 300, 30, false),
    ];
    assert_eq!(head_of(&fork), Some(id(3)));
}

#[test]
fn fork_with_equal_created_at_resolves_to_the_lowest_event_id() {
    for (low, high) in [(2_u8, 3_u8), (3, 2)] {
        let fork = [
            node(1, None, 100, 10, false),
            node(high, Some(1), 200, 30, false),
            node(low, Some(1), 200, 20, false),
        ];
        assert_eq!(head_of(&fork), Some(id(2)), "listing order {low},{high}");
    }
}

#[test]
fn deleted_head_falls_back_to_its_prev() {
    let chain = [
        node(1, None, 100, 10, false),
        node(2, Some(1), 200, 20, false),
        node(3, Some(2), 300, 30, true),
    ];
    let page = resolve_page(&chain).expect("page");
    assert_eq!(page.head.event_id, id(2));
    assert_eq!(page.head.author, id(20));
    assert_eq!(page.revision_count, 3, "soft-deleted revisions still count");
}

#[test]
fn deleted_head_walks_past_consecutive_deleted_revisions() {
    let chain = [
        node(1, None, 100, 10, false),
        node(2, Some(1), 200, 20, true),
        node(3, Some(2), 300, 30, true),
    ];
    assert_eq!(head_of(&chain), Some(id(1)));
}

#[test]
fn page_with_no_live_revision_has_no_head() {
    let chain = [
        node(1, None, 100, 10, true),
        node(2, Some(1), 200, 20, true),
    ];
    assert_eq!(resolve_page(&chain), None);
}

#[test]
fn deleted_tip_falls_back_to_its_prev_not_to_another_live_tip() {
    // NIP-PG: the head is the winning tip; if deleted, the head is its prev.
    let fork = [
        node(1, None, 100, 10, false),
        node(2, Some(1), 200, 20, false),
        node(3, Some(1), 300, 30, true),
    ];
    assert_eq!(head_of(&fork), Some(id(1)));
}

#[test]
fn revision_whose_prev_is_unknown_is_treated_as_a_root() {
    let chain = [node(2, Some(99), 200, 20, false)];
    let page = resolve_page(&chain).expect("page");
    assert_eq!(page.head.event_id, id(2));
    assert_eq!(page.created_by, id(20));
}

#[test]
fn deleted_head_with_unknown_prev_leaves_no_head() {
    let chain = [node(2, Some(99), 200, 20, true)];
    assert_eq!(resolve_page(&chain), None);
}
