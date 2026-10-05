//! Page-aware search: a page matches only through its head revision.
//!
//! NIP-PG "Search". `events.search_tsv` is generated from `content` for
//! whichever kinds the deployment's FTS policy covers, and that policy differs
//! by how the database was built: a database created by the migrations carries
//! the positive allowlist of migration 0008 (page kinds are NOT indexed), while
//! one built from `schema/schema.sql` or upgraded in place carries the negative
//! exclusion list (page kinds ARE indexed: every revision, suggestion and
//! resolution). This crate cannot change that column (fork rule 1), and no
//! policy over `events` can express "head revisions only", so page awareness
//! lives in the query plus one projection on the fork's own `pages` table:
//!
//! 1. The generic `events` arm excludes every page kind
//!    ([`push_page_kind_exclusion`]). Superseded revisions, suggestions and
//!    resolutions can therefore never match, whatever the column holds.
//! 2. A page-head arm ([`push_page_head_arm`]), `UNION ALL`ed onto the generic
//!    arm, matches `pages.search_tsv`: the title and content of each live page's
//!    head revision, maintained by a trigger on the head columns (fork-private
//!    migration `9002_pages_search.sql`) and GIN-indexed. Its cost scales with
//!    the matching pages, not with revisions or events, and it behaves the same
//!    under either FTS policy.
//!
//! The head arm is opt-in: it runs only when the query names
//! `KIND_PAGE_REVISION` in `kinds`. A kindless search keeps returning the
//! deployment's default searchable kinds (the relay's allowlist already scopes
//! it that way), and no existing client pays for the extra arm.
//!
//! Everything else (community fence, `deleted_at`, channel scope, authors,
//! since, until) is shared with the generic arm through
//! [`crate::query::push_event_filters`], and the relay still refetches and
//! re-authorizes every hit, so search never widens visibility.

use buzz_core::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION};
use sqlx::{Postgres, QueryBuilder};

use crate::query::{push_event_filters, push_tsquery, SearchQuery};

/// Every NIP-PG page kind. The generic arm excludes all of them; a test pins
/// this list to [`buzz_core::page::is_page_kind`] so a new page kind cannot be
/// added to the protocol and still leak through the generic index.
const PAGE_KINDS: [u32; 3] = [
    KIND_PAGE_REVISION,
    KIND_PAGE_SUGGESTION,
    KIND_PAGE_SUGGESTION_RESOLUTION,
];

/// Whether `query` asks for pages: `kinds` is explicit and names a page
/// revision. Suggestions and resolutions are never searchable, so naming only
/// them does not enable the head arm.
pub(crate) fn wants_page_heads(query: &SearchQuery) -> bool {
    query
        .kinds
        .as_ref()
        .is_some_and(|kinds| kinds.contains(&(KIND_PAGE_REVISION as i32)))
}

/// Exclude page kinds from the generic `events` arm.
///
/// The kinds are compile-time integer constants, so they are inlined as a
/// literal list (a constant `NOT IN` is a cheap residual filter on the bitmap
/// heap rows and keeps the GIN plan unchanged for every other kind).
pub(crate) fn push_page_kind_exclusion(qb: &mut QueryBuilder<Postgres>) {
    qb.push(" AND kind NOT IN (");
    let mut sep = qb.separated(", ");
    for kind in PAGE_KINDS {
        sep.push(kind);
    }
    qb.push(")");
}

/// Append the page-head arm (`UNION ALL SELECT ...`) when the query asks for
/// pages; otherwise push nothing.
///
/// The arm selects the same seven columns as the generic arm and its row
/// source is aliased `events`, so [`push_event_filters`] applies unchanged and
/// the arm's shape mirrors the generic one (`search_tsv @@ query` against a
/// GIN-indexed vector). The vector is the page's own (`pages.search_tsv`); the
/// head event is joined only to produce the hit's columns, and the join
/// requires it to be a live `PAGE_REVISION` in the page's own channel, so a
/// damaged index row cannot surface a suggestion, a deleted event or another
/// channel's event. Community is part of both the page predicate and the join.
///
/// The join also pins `events.created_at` to `pages.updated_at`: NIP-PG's
/// rebuild invariant stores the head event's own timestamp there, and the
/// equality lets Postgres prune `events` to the one partition holding the head
/// instead of probing every monthly partition per matching page. A row that
/// breaks the invariant simply does not match (the failure is closed).
pub(crate) fn push_page_head_arm(
    qb: &mut QueryBuilder<Postgres>,
    query: &SearchQuery,
    search_text: &str,
) {
    if !wants_page_heads(query) {
        return;
    }
    qb.push(
        " UNION ALL SELECT id, kind, pubkey, channel_id, created_at, \
         EXTRACT(EPOCH FROM created_at)::bigint AS created_at_s, \
         ts_rank_cd(search_tsv, search_query.query) AS rank \
         FROM (SELECT e.id, e.community_id, e.kind, e.pubkey, e.channel_id, \
                      e.created_at, e.deleted_at, p.search_tsv \
               FROM pages p JOIN events e \
                 ON e.community_id = p.community_id \
                AND e.id = p.head_event_id \
                AND e.created_at = p.updated_at \
                AND e.channel_id = p.channel_id \
               WHERE p.community_id = ",
    );
    qb.push_bind(*query.community.as_uuid());
    qb.push(" AND p.deleted_at IS NULL AND e.deleted_at IS NULL AND e.kind = ");
    qb.push_bind(KIND_PAGE_REVISION as i32);
    qb.push(") AS events CROSS JOIN LATERAL (SELECT ");
    push_tsquery(qb, query.mode, search_text);
    qb.push(" AS query) AS search_query WHERE community_id = ");
    qb.push_bind(*query.community.as_uuid());
    qb.push(" AND deleted_at IS NULL AND search_tsv @@ search_query.query");
    push_event_filters(qb, query);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{ChannelScope, SearchMode};
    use buzz_core::{page::is_page_kind, CommunityId};

    fn query_with_kinds(kinds: Option<Vec<i32>>) -> SearchQuery {
        SearchQuery {
            community: CommunityId::from_uuid(uuid::Uuid::nil()),
            q: "needle".into(),
            channel_scope: ChannelScope::Any,
            kinds,
            authors: None,
            since: None,
            until: None,
            page: 1,
            per_page: 10,
            mode: SearchMode::FullText,
        }
    }

    #[test]
    fn exclusion_list_covers_every_protocol_page_kind() {
        // Walk every u16 kind: anything the protocol calls a page kind must be
        // in the exclusion list, and nothing else may be.
        for kind in 0..=u32::from(u16::MAX) {
            assert_eq!(
                PAGE_KINDS.contains(&kind),
                is_page_kind(kind),
                "kind {kind}: exclusion list and is_page_kind disagree"
            );
        }
    }

    #[test]
    fn head_arm_is_opt_in_by_naming_the_revision_kind() {
        assert!(!wants_page_heads(&query_with_kinds(None)));
        assert!(!wants_page_heads(&query_with_kinds(Some(vec![]))));
        assert!(!wants_page_heads(&query_with_kinds(Some(vec![9, 40002]))));
        // Suggestions and resolutions never match, so they never enable it.
        assert!(!wants_page_heads(&query_with_kinds(Some(vec![
            KIND_PAGE_SUGGESTION as i32,
            KIND_PAGE_SUGGESTION_RESOLUTION as i32,
        ]))));
        assert!(wants_page_heads(&query_with_kinds(Some(vec![
            KIND_PAGE_REVISION as i32
        ]))));
        assert!(wants_page_heads(&query_with_kinds(Some(vec![
            9,
            KIND_PAGE_REVISION as i32
        ]))));
    }

    #[test]
    fn exclusion_sql_lists_all_page_kinds() {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("");
        push_page_kind_exclusion(&mut qb);
        assert_eq!(qb.sql(), " AND kind NOT IN (52000, 52001, 52002)");
    }
}
