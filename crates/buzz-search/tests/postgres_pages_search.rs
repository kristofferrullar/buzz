//! Integration tests for page-aware search (NIP-PG "Search").
//!
//! A page matches search only through its head revision. Superseded revisions,
//! suggestions, resolutions and soft-deleted pages or events never match, and
//! non-page search is untouched.
//!
//! Run with a local PG: `BUZZ_TEST_DATABASE_URL=postgres://buzz:buzz_dev@localhost:5432/buzz cargo test -p buzz-search --test postgres_pages_search -- --include-ignored`
//!
//! Each test builds a private schema from the real migrations (the same chain
//! `postgres_fts_integration.rs` applies, plus the fork-private `pages` index
//! migration 9001 verbatim), and runs its scenario under BOTH full-text
//! policies a database can carry:
//!
//! * `Allowlist`: what the migrations build on a fresh install (migration
//!   0008). Page kinds are NOT indexed, so `events.search_tsv` is NULL for them.
//! * `ExclusionList`: what `schema/schema.sql` and in-place upgrades carry.
//!   Page kinds ARE indexed: every revision, suggestion and resolution.
//!
//! The head-only guarantee must hold under both, so no scenario may depend on
//! which one it runs on. Only the `pages` write fence (migration 0029) is
//! stubbed: it is irrelevant to search and 0029 needs the whole migration
//! chain.

use std::future::Future;
use std::pin::Pin;

use buzz_core::{
    kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION},
    CommunityId,
};
use buzz_search::{ChannelScope, SearchHit, SearchMode, SearchQuery, SearchService};
use sqlx::{postgres::PgPoolOptions, Executor, PgPool, Row};
use uuid::Uuid;

const TEST_DB_URL: &str = "postgres://buzz:buzz_dev@localhost:5432/buzz";
const MIGRATION_0001_SQL: &str = include_str!("../../../migrations/0001_initial_schema.sql");
const MIGRATION_0002_SQL: &str = include_str!("../../../migrations/0002_git_repo_names.sql");
const MIGRATION_0003_SQL: &str = include_str!("../../../migrations/0003_community_icon.sql");
const MIGRATION_0004_SQL: &str = include_str!("../../../migrations/0004_events_tags_gin.sql");
const MIGRATION_0005_SQL: &str = include_str!("../../../migrations/0005_agent_turn_metric_fts.sql");
const MIGRATION_0006_SQL: &str = include_str!("../../../migrations/0006_moderation.sql");
const MIGRATION_0007_SQL: &str = include_str!("../../../migrations/0007_nip_rs_retention.sql");
const MIGRATION_0008_SQL: &str =
    include_str!("../../../migrations/0008_fresh_install_search_allowlist.sql");
const MIGRATION_0014_SQL: &str = include_str!("../../../migrations/0014_push_lease_fts.sql");
const MIGRATION_0033_SQL: &str =
    include_str!("../../../migrations/0033_private_managed_agent_fts.sql");
const MIGRATION_9001_SQL: &str = include_str!("../../../migrations/9001_pages_index.sql");
const MIGRATION_9002_SQL: &str = include_str!("../../../migrations/9002_pages_search.sql");

/// `schema/schema.sql`'s `search_tsv` expression, installed over the
/// allowlist to model a database that indexes every kind not excluded.
const EXCLUSION_LIST_FTS_SQL: &str = "
    ALTER TABLE events DROP COLUMN search_tsv;
    ALTER TABLE events ADD COLUMN search_tsv TSVECTOR GENERATED ALWAYS AS (
        CASE WHEN kind IN (1059, 30179, 30300, 30350, 30622, 44100, 44101, 44200) THEN NULL::tsvector
             ELSE to_tsvector('simple', content)
        END
    ) STORED;
    CREATE INDEX idx_events_search_tsv ON events USING GIN (search_tsv);
";

/// 2023-11-14, comfortably inside the `events_p_past` partition.
const T0: i64 = 1_700_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FtsPolicy {
    Allowlist,
    ExclusionList,
}

const POLICIES: [FtsPolicy; 2] = [FtsPolicy::Allowlist, FtsPolicy::ExclusionList];

fn test_db_url() -> String {
    std::env::var("BUZZ_TEST_DATABASE_URL").unwrap_or_else(|_| TEST_DB_URL.to_string())
}

async fn setup(policy: FtsPolicy) -> (PgPool, String) {
    let url = test_db_url();
    let schema = format!("pgs_test_{}", Uuid::new_v4().simple());
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("connect");
    let create_sql = format!("CREATE SCHEMA \"{schema}\"");
    sqlx::query(sqlx::AssertSqlSafe(create_sql))
        .execute(&admin_pool)
        .await
        .expect("create schema");
    admin_pool.close().await;

    let url_with_search_path = format!("{url}?options=-c%20search_path%3D{schema}");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url_with_search_path)
        .await
        .expect("connect with search_path");
    for (name, sql) in [
        ("0001", MIGRATION_0001_SQL),
        ("0002", MIGRATION_0002_SQL),
        ("0003", MIGRATION_0003_SQL),
        ("0004", MIGRATION_0004_SQL),
        ("0005", MIGRATION_0005_SQL),
        ("0006", MIGRATION_0006_SQL),
        ("0007", MIGRATION_0007_SQL),
        ("0008", MIGRATION_0008_SQL),
        ("0014", MIGRATION_0014_SQL),
        ("0033", MIGRATION_0033_SQL),
    ] {
        pool.execute(sql)
            .await
            .unwrap_or_else(|e| panic!("apply {name} migration: {e}"));
    }
    // Migration 9001 calls the community write fence helper (0029), which
    // needs the full chain. The fence is irrelevant to search, so stub it.
    pool.execute(
        "CREATE FUNCTION attach_community_write_fence(target REGCLASS) RETURNS VOID \
         LANGUAGE sql AS 'SELECT'",
    )
    .await
    .expect("stub fence helper");
    pool.execute(MIGRATION_9001_SQL)
        .await
        .expect("apply 9001 pages migration");
    pool.execute(MIGRATION_9002_SQL)
        .await
        .expect("apply 9002 page search migration");
    if policy == FtsPolicy::ExclusionList {
        pool.execute(EXCLUSION_LIST_FTS_SQL)
            .await
            .expect("install exclusion-list FTS policy");
    }
    (pool, schema)
}

async fn teardown(pool: PgPool, schema: &str) {
    pool.close().await;
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&test_db_url())
        .await
        .expect("reconnect for drop");
    let drop_sql = format!("DROP SCHEMA \"{schema}\" CASCADE");
    sqlx::query(sqlx::AssertSqlSafe(drop_sql))
        .execute(&admin_pool)
        .await
        .expect("drop schema");
    admin_pool.close().await;
}

type Scenario = for<'a> fn(&'a PgPool, FtsPolicy) -> Pin<Box<dyn Future<Output = ()> + 'a>>;

/// Run `scenario` on a fresh schema under every FTS policy.
async fn under_each_policy(scenario: Scenario) {
    for policy in POLICIES {
        let (pool, schema) = setup(policy).await;
        scenario(&pool, policy).await;
        teardown(pool, &schema).await;
    }
}

// -- Fixtures ------------------------------------------------------------------

fn rand_bytes32() -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = Uuid::new_v4();
    out[..16].copy_from_slice(bytes.as_bytes());
    out[16..].copy_from_slice(bytes.as_bytes());
    out
}

async fn mk_community(pool: &PgPool, host: &str) -> CommunityId {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO communities (id, host, signing_key) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(host)
        .bind(b"signingkey".as_slice())
        .execute(pool)
        .await
        .expect("insert community");
    CommunityId::from_uuid(id)
}

async fn mk_channel(pool: &PgPool, community: CommunityId) -> Uuid {
    mk_channel_with_id(pool, community, Uuid::new_v4()).await
}

/// A channel with a chosen id (the same uuid may exist in two communities).
async fn mk_channel_with_id(pool: &PgPool, community: CommunityId, id: Uuid) -> Uuid {
    sqlx::query(
        "INSERT INTO channels (id, community_id, name, created_by) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(community.as_uuid())
    .bind(format!("chan-{}", id.simple()))
    .bind(rand_bytes32().as_slice())
    .execute(pool)
    .await
    .expect("insert channel");
    id
}

#[allow(clippy::too_many_arguments)]
async fn insert_event(
    pool: &PgPool,
    community: CommunityId,
    id: [u8; 32],
    pubkey: [u8; 32],
    kind: i32,
    content: &str,
    channel_id: Option<Uuid>,
    d_tag: Option<Uuid>,
    created_at_secs: i64,
) {
    sqlx::query(
        "INSERT INTO events (community_id, id, pubkey, created_at, kind, tags, content, sig, channel_id, d_tag) \
         VALUES ($1, $2, $3, to_timestamp($4), $5, '[]'::jsonb, $6, $7, $8, $9)",
    )
    .bind(community.as_uuid())
    .bind(&id[..])
    .bind(&pubkey[..])
    .bind(created_at_secs)
    .bind(kind)
    .bind(content)
    .bind(b"signature".as_slice())
    .bind(channel_id)
    .bind(d_tag.map(|d| d.to_string()))
    .execute(pool)
    .await
    .expect("insert event");
}

/// One page in one channel. Mirrors what the relay's page ingest writes: the
/// event rows plus a `pages` index row naming the head.
struct Page {
    community: CommunityId,
    channel: Uuid,
    id: Uuid,
    author: [u8; 32],
    /// Revision event ids, oldest first.
    revisions: Vec<[u8; 32]>,
}

impl Page {
    fn new(community: CommunityId, channel: Uuid) -> Self {
        Self {
            community,
            channel,
            id: Uuid::new_v4(),
            author: rand_bytes32(),
            revisions: Vec::new(),
        }
    }

    /// Store a revision event at `created_at` and make it the page head, exactly
    /// as an accepted `PAGE_REVISION` does (event insert + head upsert).
    async fn revise(&mut self, pool: &PgPool, content: &str, created_at: i64) -> [u8; 32] {
        self.revise_titled(pool, "Page title", content, created_at)
            .await
    }

    /// [`Page::revise`] with an explicit title.
    async fn revise_titled(
        &mut self,
        pool: &PgPool,
        title: &str,
        content: &str,
        created_at: i64,
    ) -> [u8; 32] {
        let id = rand_bytes32();
        insert_event(
            pool,
            self.community,
            id,
            self.author,
            KIND_PAGE_REVISION as i32,
            content,
            Some(self.channel),
            Some(self.id),
            created_at,
        )
        .await;
        sqlx::query(
            "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
                 created_by, created_at, updated_by, updated_at) \
             VALUES ($1, $2, $3, $4, $7, $5, to_timestamp($6), $5, to_timestamp($6)) \
             ON CONFLICT (community_id, channel_id, page_id) DO UPDATE SET \
                 head_event_id = EXCLUDED.head_event_id, \
                 title = EXCLUDED.title, \
                 updated_by = EXCLUDED.updated_by, \
                 updated_at = EXCLUDED.updated_at, \
                 revision_count = pages.revision_count + 1",
        )
        .bind(self.community.as_uuid())
        .bind(self.channel)
        .bind(self.id)
        .bind(&id[..])
        .bind(&self.author[..])
        .bind(created_at)
        .bind(title)
        .execute(pool)
        .await
        .expect("upsert page head");
        self.revisions.push(id);
        id
    }

    /// Store a non-head page event (suggestion or resolution) for this page.
    async fn side_event(
        &self,
        pool: &PgPool,
        kind: u32,
        content: &str,
        created_at: i64,
    ) -> [u8; 32] {
        let id = rand_bytes32();
        insert_event(
            pool,
            self.community,
            id,
            rand_bytes32(),
            kind as i32,
            content,
            Some(self.channel),
            Some(self.id),
            created_at,
        )
        .await;
        id
    }

    async fn tombstone(&self, pool: &PgPool) {
        sqlx::query(
            "UPDATE pages SET deleted_at = NOW() \
             WHERE community_id = $1 AND channel_id = $2 AND page_id = $3",
        )
        .bind(self.community.as_uuid())
        .bind(self.channel)
        .bind(self.id)
        .execute(pool)
        .await
        .expect("tombstone page");
    }
}

/// Insert a `pages` row WITHOUT the search trigger, carrying a vector the
/// trigger would never produce. Models index damage (a stale or hand-edited
/// row) so the query's own guards (community, channel, live revision) are
/// tested independently of the projection that normally prevents it.
async fn insert_damaged_page_row(
    pool: &PgPool,
    community: CommunityId,
    channel: Uuid,
    page_id: Uuid,
    head: [u8; 32],
    created_at: i64,
    searchable_text: &str,
) {
    pool.execute("ALTER TABLE pages DISABLE TRIGGER pages_search_tsv")
        .await
        .expect("disable search trigger");
    sqlx::query(
        "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
             created_by, created_at, updated_by, updated_at, search_tsv) \
         VALUES ($1, $2, $3, $4, 't', $5, to_timestamp($6), $5, to_timestamp($6), \
                 to_tsvector('simple', $7))",
    )
    .bind(community.as_uuid())
    .bind(channel)
    .bind(page_id)
    .bind(&head[..])
    .bind(rand_bytes32().as_slice())
    .bind(created_at)
    .bind(searchable_text)
    .execute(pool)
    .await
    .expect("insert damaged index row");
    pool.execute("ALTER TABLE pages ENABLE TRIGGER pages_search_tsv")
        .await
        .expect("re-enable search trigger");
}

async fn soft_delete_event(pool: &PgPool, community: CommunityId, id: [u8; 32]) {
    sqlx::query("UPDATE events SET deleted_at = NOW() WHERE community_id = $1 AND id = $2")
        .bind(community.as_uuid())
        .bind(&id[..])
        .execute(pool)
        .await
        .expect("soft delete event");
}

fn query(community: CommunityId, q: &str, kinds: Option<Vec<i32>>) -> SearchQuery {
    SearchQuery {
        community,
        q: q.to_string(),
        channel_scope: ChannelScope::Any,
        kinds,
        authors: None,
        since: None,
        until: None,
        page: 1,
        per_page: 50,
        mode: SearchMode::FullText,
    }
}

fn pages_kinds() -> Option<Vec<i32>> {
    Some(vec![KIND_PAGE_REVISION as i32])
}

async fn run(pool: &PgPool, q: &SearchQuery) -> Vec<SearchHit> {
    SearchService::new(pool.clone())
        .search(q)
        .await
        .expect("search ok")
        .hits
}

fn hit_ids(hits: &[SearchHit]) -> Vec<[u8; 32]> {
    hits.iter().map(|h| h.event_id).collect()
}

// -- Premise -------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Postgres"]
async fn fixture_spans_both_fts_policies() {
    // The head-only guarantee is only meaningful if the two fixtures really
    // differ in whether the generic index covers page kinds.
    for policy in POLICIES {
        let (pool, schema) = setup(policy).await;
        let community = mk_community(&pool, "premise.example").await;
        let channel = mk_channel(&pool, community).await;
        let mut page = Page::new(community, channel);
        let rev = page.revise(&pool, "premise token", T0).await;
        let indexed: bool = sqlx::query(
            "SELECT search_tsv IS NOT NULL AS indexed FROM events WHERE community_id = $1 AND id = $2",
        )
        .bind(community.as_uuid())
        .bind(&rev[..])
        .fetch_one(&pool)
        .await
        .expect("read tsv")
        .get("indexed");
        assert_eq!(
            indexed,
            policy == FtsPolicy::ExclusionList,
            "{policy:?}: unexpected generic-index coverage of page kinds"
        );
        // ...while the page's own vector is policy-independent.
        let page_indexed: bool = sqlx::query(
            "SELECT search_tsv IS NOT NULL AS indexed FROM pages WHERE community_id = $1",
        )
        .bind(community.as_uuid())
        .fetch_one(&pool)
        .await
        .expect("read page tsv")
        .get("indexed");
        assert!(page_indexed, "{policy:?}: pages.search_tsv must be set");
        teardown(pool, &schema).await;
    }
}

// -- Head-only matching ----------------------------------------------------------

fn only_the_head_revision_matches<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "head.example").await;
        let channel = mk_channel(pool, community).await;
        let mut page = Page::new(community, channel);
        // Three revisions; each has a token unique to it and one shared token.
        page.revise(pool, "alphaone sharedword", T0).await;
        page.revise(pool, "bravotwo sharedword", T0 + 10).await;
        let head = page.revise(pool, "charliethree sharedword", T0 + 20).await;

        let hits = run(pool, &query(community, "charliethree", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![head], "{policy:?}: the head matches");
        assert_eq!(hits[0].kind, KIND_PAGE_REVISION as i32);
        assert_eq!(hits[0].channel_id, Some(channel));
        assert_eq!(hits[0].created_at, T0 + 20);
        assert!(hits[0].rank > 0.0);

        for superseded in ["alphaone", "bravotwo"] {
            let hits = run(pool, &query(community, superseded, pages_kinds())).await;
            assert!(
                hits.is_empty(),
                "{policy:?}: superseded text {superseded:?} must not match"
            );
        }

        // A token every revision shares matches the page once, not three times.
        let hits = run(pool, &query(community, "sharedword", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![head], "{policy:?}: one hit per page");
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn page_with_three_revisions_matches_only_through_its_head() {
    under_each_policy(only_the_head_revision_matches).await;
}

fn an_edit_changes_what_matches<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "edit.example").await;
        let channel = mk_channel(pool, community).await;
        let mut page = Page::new(community, channel);
        let first = page.revise(pool, "firstdraft text", T0).await;

        let hits = run(pool, &query(community, "firstdraft", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![first], "{policy:?}");

        let second = page.revise(pool, "seconddraft text", T0 + 5).await;
        assert!(
            run(pool, &query(community, "firstdraft", pages_kinds()))
                .await
                .is_empty(),
            "{policy:?}: the replaced head stops matching"
        );
        let hits = run(pool, &query(community, "seconddraft", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![second], "{policy:?}");
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn editing_a_page_moves_the_match_to_the_new_head() {
    under_each_policy(an_edit_changes_what_matches).await;
}

fn head_is_the_pointer_not_the_newest_timestamp<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        // A revision chained on `a` can carry an OLDER created_at than `a` (a
        // writer with a slow clock): the relay accepts it and it becomes the
        // head. "Newest revision by created_at" would pick the wrong event.
        let community = mk_community(pool, "skew.example").await;
        let channel = mk_channel(pool, community).await;
        let mut page = Page::new(community, channel);
        let newest_by_time = page.revise(pool, "newesttime body", T0 + 100).await;
        let head = page.revise(pool, "slowclock body", T0 + 50).await;

        let hits = run(pool, &query(community, "slowclock", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![head], "{policy:?}: the head matches");
        let hits = run(pool, &query(community, "newesttime", pages_kinds())).await;
        assert!(
            hits.is_empty(),
            "{policy:?}: newest-by-created_at {newest_by_time:?} is not the head and must not match"
        );
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn head_means_the_index_pointer_not_the_newest_created_at() {
    under_each_policy(head_is_the_pointer_not_the_newest_timestamp).await;
}

fn the_head_title_is_searchable<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "title.example").await;
        let channel = mk_channel(pool, community).await;
        let mut page = Page::new(community, channel);
        page.revise_titled(pool, "Oldtitleword", "body one", T0)
            .await;
        let head = page
            .revise_titled(pool, "Quarterlyroadmap", "body two", T0 + 1)
            .await;

        let hits = run(pool, &query(community, "quarterlyroadmap", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![head], "{policy:?}: head title matches");
        assert!(
            run(pool, &query(community, "oldtitleword", pages_kinds()))
                .await
                .is_empty(),
            "{policy:?}: a superseded title must not match"
        );
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_title_of_the_head_revision_is_searchable() {
    under_each_policy(the_head_title_is_searchable).await;
}

fn the_projection_names_only_a_live_head_revision<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "projection.example").await;
        let other = mk_community(pool, "projection-other.example").await;
        let channel = mk_channel(pool, community).await;
        let other_channel = mk_channel(pool, other).await;

        async fn page_tsv_text(pool: &PgPool, page_id: Uuid) -> Option<String> {
            sqlx::query("SELECT search_tsv::text AS t FROM pages WHERE page_id = $1")
                .bind(page_id)
                .fetch_one(pool)
                .await
                .expect("read page vector")
                .get("t")
        }
        async fn plain_insert(
            pool: &PgPool,
            community: CommunityId,
            channel: Uuid,
            page_id: Uuid,
            head: [u8; 32],
        ) {
            sqlx::query(
                "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
                     created_by, created_at, updated_by, updated_at) \
                 VALUES ($1, $2, $3, $4, 'Title', $5, to_timestamp($6), $5, to_timestamp($6))",
            )
            .bind(community.as_uuid())
            .bind(channel)
            .bind(page_id)
            .bind(&head[..])
            .bind(rand_bytes32().as_slice())
            .bind(T0)
            .execute(pool)
            .await
            .expect("insert page row");
        }

        // A live revision head: title (A) and content are projected.
        let mut live = Page::new(community, channel);
        live.revise_titled(pool, "Livetitle", "livebody", T0).await;
        let text = page_tsv_text(pool, live.id).await.expect("vector set");
        assert!(
            text.contains("'livetitle':1A") && text.contains("'livebody'"),
            "{policy:?}: {text}"
        );

        // A head that is a suggestion, in another community, missing, or
        // deleted projects NOTHING (NULL never matches): the index fails closed.
        let suggestion_page = Page::new(community, channel);
        let suggestion = suggestion_page
            .side_event(pool, KIND_PAGE_SUGGESTION, "suggestionbody", T0)
            .await;
        plain_insert(pool, community, channel, suggestion_page.id, suggestion).await;
        assert_eq!(page_tsv_text(pool, suggestion_page.id).await, None);

        let foreign_page = Page::new(other, other_channel);
        let mut owner = Page::new(community, channel);
        let foreign_head = owner.revise(pool, "foreignbody", T0).await;
        plain_insert(pool, other, other_channel, foreign_page.id, foreign_head).await;
        assert_eq!(page_tsv_text(pool, foreign_page.id).await, None);

        let missing_page = Page::new(community, channel);
        plain_insert(pool, community, channel, missing_page.id, rand_bytes32()).await;
        assert_eq!(page_tsv_text(pool, missing_page.id).await, None);

        let mut deleted = Page::new(community, channel);
        let deleted_head = deleted.revise(pool, "deletedbody", T0).await;
        soft_delete_event(pool, community, deleted_head).await;
        deleted.revise(pool, "revivedbody", T0 + 1).await;
        // Re-pointing the head re-projects: the new head's text replaces it.
        let text = page_tsv_text(pool, deleted.id).await.expect("vector set");
        assert!(text.contains("'revivedbody'") && !text.contains("deletedbody"));
        // Pointing the head back at the deleted event (with its own timestamp,
        // so only the deletion can explain the result) projects nothing.
        sqlx::query(
            "UPDATE pages SET head_event_id = $1, updated_at = to_timestamp($2) WHERE page_id = $3",
        )
        .bind(&deleted_head[..])
        .bind(T0)
        .bind(deleted.id)
        .execute(pool)
        .await
        .expect("point head at a deleted event");
        assert_eq!(page_tsv_text(pool, deleted.id).await, None);

        // The index stores the head event's own timestamp (NIP-PG rebuild
        // invariant); a row that does not is not projected (fail closed).
        let mut skewed = Page::new(community, channel);
        let skewed_head = skewed.revise(pool, "skewedbody", T0).await;
        assert!(page_tsv_text(pool, skewed.id).await.is_some());
        sqlx::query(
            "UPDATE pages SET head_event_id = $1, updated_at = to_timestamp($2) WHERE page_id = $3",
        )
        .bind(&skewed_head[..])
        .bind(T0 + 99)
        .bind(skewed.id)
        .execute(pool)
        .await
        .expect("skew updated_at");
        assert_eq!(page_tsv_text(pool, skewed.id).await, None);
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn the_search_projection_names_only_a_live_head_revision() {
    under_each_policy(the_projection_names_only_a_live_head_revision).await;
}

// -- Suggestions and resolutions ---------------------------------------------------

fn suggestions_and_resolutions_never_match<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "suggest.example").await;
        let channel = mk_channel(pool, community).await;
        let mut page = Page::new(community, channel);
        let head = page.revise(pool, "pagebody", T0).await;
        page.side_event(pool, KIND_PAGE_SUGGESTION, "suggestedtext rewrite", T0 + 1)
            .await;
        page.side_event(
            pool,
            KIND_PAGE_SUGGESTION_RESOLUTION,
            "resolvedtext note",
            T0 + 2,
        )
        .await;

        // A control message proves the same tokens are searchable elsewhere.
        let control = rand_bytes32();
        insert_event(
            pool,
            community,
            control,
            rand_bytes32(),
            9,
            "suggestedtext resolvedtext chat",
            Some(channel),
            None,
            T0 + 3,
        )
        .await;

        let page_and_side_kinds = Some(vec![
            KIND_PAGE_REVISION as i32,
            KIND_PAGE_SUGGESTION as i32,
            KIND_PAGE_SUGGESTION_RESOLUTION as i32,
        ]);
        for q in ["suggestedtext", "resolvedtext"] {
            for kinds in [
                None,
                Some(vec![KIND_PAGE_SUGGESTION as i32]),
                Some(vec![KIND_PAGE_SUGGESTION_RESOLUTION as i32]),
                page_and_side_kinds.clone(),
            ] {
                let hits = run(pool, &query(community, q, kinds.clone())).await;
                assert!(
                    hits.iter().all(|h| h.kind == 9),
                    "{policy:?}: {q:?} with kinds {kinds:?} matched a page side event: {hits:?}"
                );
            }
        }
        // The control chat message is still found, and the head still matches.
        let hits = run(pool, &query(community, "suggestedtext", None)).await;
        assert_eq!(hit_ids(&hits), vec![control], "{policy:?}");
        let hits = run(pool, &query(community, "pagebody", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![head], "{policy:?}");
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn suggestion_and_resolution_text_never_matches() {
    under_each_policy(suggestions_and_resolutions_never_match).await;
}

// -- Deletion ------------------------------------------------------------------

fn deleted_pages_and_events_never_match<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "delete.example").await;
        let channel = mk_channel(pool, community).await;

        // A tombstoned page (its events are soft-deleted in the same operation
        // in production; here only the index tombstone is set, so this guards
        // the `pages.deleted_at` predicate on its own).
        let mut tombstoned = Page::new(community, channel);
        tombstoned.revise(pool, "tombstonedtext", T0).await;
        assert_eq!(
            run(pool, &query(community, "tombstonedtext", pages_kinds()))
                .await
                .len(),
            1,
            "{policy:?}: live page matches before deletion"
        );
        tombstoned.tombstone(pool).await;
        assert!(
            run(pool, &query(community, "tombstonedtext", pages_kinds()))
                .await
                .is_empty(),
            "{policy:?}: a tombstoned page must not match"
        );

        // A live page index row whose head event was soft-deleted (the index
        // never names a deleted event, but search must not depend on that).
        let mut deleted_head = Page::new(community, channel);
        let head = deleted_head.revise(pool, "deletedheadtext", T0 + 1).await;
        soft_delete_event(pool, community, head).await;
        assert!(
            run(pool, &query(community, "deletedheadtext", pages_kinds()))
                .await
                .is_empty(),
            "{policy:?}: a deleted head event must not match"
        );
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn deleted_pages_and_deleted_head_events_never_match() {
    under_each_policy(deleted_pages_and_events_never_match).await;
}

// -- Isolation -------------------------------------------------------------------

fn community_isolation<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let a = mk_community(pool, "iso-a.example").await;
        let b = mk_community(pool, "iso-b.example").await;
        let chan_a = mk_channel(pool, a).await;
        let chan_b = mk_channel(pool, b).await;
        let mut page_a = Page::new(a, chan_a);
        let head_a = page_a.revise(pool, "isolatedtoken in a", T0).await;
        let mut page_b = Page::new(b, chan_b);
        let head_b = page_b.revise(pool, "isolatedtoken in b", T0).await;

        let hits_a = run(pool, &query(a, "isolatedtoken", pages_kinds())).await;
        assert_eq!(hit_ids(&hits_a), vec![head_a], "{policy:?}: A sees only A");
        let hits_b = run(pool, &query(b, "isolatedtoken", pages_kinds())).await;
        assert_eq!(hit_ids(&hits_b), vec![head_b], "{policy:?}: B sees only B");

        // A damaged index row in B that names A's head event id must not
        // surface A's content to B: the community fence applies to the head
        // event itself, not only to the page row. The row reuses A's channel
        // uuid (legal: a channel id may exist in two communities) and A's head
        // timestamp, so every other guard (channel, kind, time) would pass.
        let chan_b2 = mk_channel_with_id(pool, b, chan_a).await;
        insert_damaged_page_row(
            pool,
            b,
            chan_b2,
            Uuid::new_v4(),
            head_a,
            T0,
            "isolatedtoken in a",
        )
        .await;
        let hits_b = run(pool, &query(b, "isolatedtoken", pages_kinds())).await;
        assert_eq!(
            hit_ids(&hits_b),
            vec![head_b],
            "{policy:?}: B must not see A's event through a mis-scoped index row"
        );
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn page_search_is_community_isolated() {
    under_each_policy(community_isolation).await;
}

// -- Channel access --------------------------------------------------------------

fn channel_scope_applies_to_pages<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "scope.example").await;
        let readable = mk_channel(pool, community).await;
        let private = mk_channel(pool, community).await;
        let mut visible = Page::new(community, readable);
        let visible_head = visible.revise(pool, "scopedtoken readable", T0).await;
        let mut hidden = Page::new(community, private);
        let hidden_head = hidden.revise(pool, "scopedtoken private", T0 + 1).await;

        // The viewer can read only `readable`: that is what the relay passes
        // (its accessible-channel set), for REQ and for /query alike.
        for scope in [
            ChannelScope::Channels(vec![readable]),
            ChannelScope::ChannelsOrChannelLess(vec![readable]),
        ] {
            let mut q = query(community, "scopedtoken", pages_kinds());
            q.channel_scope = scope.clone();
            let hits = run(pool, &q).await;
            assert_eq!(
                hit_ids(&hits),
                vec![visible_head],
                "{policy:?}: {scope:?} must hide the private channel's page"
            );
        }

        // No readable channels at all: only channel-less rows are in scope, and
        // a page always lives in a channel.
        let mut q = query(community, "scopedtoken", pages_kinds());
        q.channel_scope = ChannelScope::ChannelLessOnly;
        assert!(
            run(pool, &q).await.is_empty(),
            "{policy:?}: channel-less only"
        );

        // A channel the viewer cannot read is not searchable even when named.
        let mut q = query(community, "scopedtoken", pages_kinds());
        q.channel_scope = ChannelScope::Channels(vec![Uuid::new_v4()]);
        assert!(
            run(pool, &q).await.is_empty(),
            "{policy:?}: unknown channel"
        );

        // Sanity: with the whole community in scope both pages are found.
        let hits = run(pool, &query(community, "scopedtoken", pages_kinds())).await;
        let mut got = hit_ids(&hits);
        got.sort();
        let mut want = vec![visible_head, hidden_head];
        want.sort();
        assert_eq!(got, want, "{policy:?}");
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_channel_the_viewer_cannot_read_yields_no_page_hits() {
    under_each_policy(channel_scope_applies_to_pages).await;
}

// -- Damaged index rows ------------------------------------------------------------

fn damaged_index_rows_scenario<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "damaged.example").await;
        let chan_a = mk_channel(pool, community).await;
        let chan_b = mk_channel(pool, community).await;

        // (1) The head names an event of a different channel than the page's.
        let mut page = Page::new(community, chan_a);
        let rev = page.revise(pool, "wrongchanneltext", T0).await;
        sqlx::query("UPDATE pages SET channel_id = $1 WHERE community_id = $2 AND page_id = $3")
            .bind(chan_b)
            .bind(community.as_uuid())
            .bind(page.id)
            .execute(pool)
            .await
            .expect("re-home index row");
        for scope in [Some(chan_a), Some(chan_b), None] {
            let mut q = query(community, "wrongchanneltext", pages_kinds());
            if let Some(ch) = scope {
                q.channel_scope = ChannelScope::Channels(vec![ch]);
            }
            assert!(
                run(pool, &q).await.is_empty(),
                "{policy:?}: head {rev:?} in another channel than its page must not match ({scope:?})"
            );
        }

        // (2) The head names a suggestion, not a revision.
        let page2 = Page::new(community, chan_a);
        let suggestion = page2
            .side_event(pool, KIND_PAGE_SUGGESTION, "headissuggestion text", T0 + 1)
            .await;
        insert_damaged_page_row(
            pool,
            community,
            chan_a,
            page2.id,
            suggestion,
            T0 + 1,
            "headissuggestion text",
        )
        .await;
        let hits = run(
            pool,
            &query(
                community,
                "headissuggestion",
                Some(vec![KIND_PAGE_REVISION as i32, KIND_PAGE_SUGGESTION as i32]),
            ),
        )
        .await;
        assert!(hits.is_empty(), "{policy:?}: a suggestion is never a head");
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn a_damaged_index_row_cannot_surface_a_wrong_event() {
    under_each_policy(damaged_index_rows_scenario).await;
}

// -- Query shape: filters, modes, paging, mixed kinds ----------------------------------

fn filters_modes_and_paging_scenario<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "shape.example").await;
        let channel = mk_channel(pool, community).await;

        let mut p1 = Page::new(community, channel);
        let h1 = p1.revise(pool, "shapetoken one", T0).await;
        let mut p2 = Page::new(community, channel);
        let h2 = p2.revise(pool, "shapetoken two", T0 + 100).await;
        let mut p3 = Page::new(community, channel);
        let h3 = p3.revise(pool, "shapetoken three", T0 + 200).await;

        // authors: the head's author.
        let mut q = query(community, "shapetoken", pages_kinds());
        q.authors = Some(vec![p2.author.to_vec()]);
        assert_eq!(
            hit_ids(&run(pool, &q).await),
            vec![h2],
            "{policy:?}: authors"
        );

        // since / until bound the head revision's created_at.
        let mut q = query(community, "shapetoken", pages_kinds());
        q.since = Some(T0 + 50);
        q.until = Some(T0 + 150);
        assert_eq!(
            hit_ids(&run(pool, &q).await),
            vec![h2],
            "{policy:?}: since/until"
        );

        // Prefix (typeahead) mode matches the head by a token prefix.
        let mut q = query(community, "shapetok", pages_kinds());
        q.mode = SearchMode::Prefix;
        let mut got = hit_ids(&run(pool, &q).await);
        got.sort();
        let mut want = vec![h1, h2, h3];
        want.sort();
        assert_eq!(got, want, "{policy:?}: prefix mode");

        // Paging walks the full set exactly once: equal ranks order by
        // created_at desc then id, so page k is a stable slice.
        let mut seen = Vec::new();
        for page in 1..=3 {
            let mut q = query(community, "shapetoken", pages_kinds());
            q.per_page = 1;
            q.page = page;
            seen.extend(hit_ids(&run(pool, &q).await));
        }
        assert_eq!(seen, vec![h3, h2, h1], "{policy:?}: paging newest first");

        // Mixed kinds: chat messages and page heads interleave in one result
        // set, and page 2 continues page 1 without repeats.
        let msg_a = rand_bytes32();
        insert_event(
            pool,
            community,
            msg_a,
            rand_bytes32(),
            9,
            "shapetoken chat a",
            Some(channel),
            None,
            T0 + 300,
        )
        .await;
        let msg_b = rand_bytes32();
        insert_event(
            pool,
            community,
            msg_b,
            rand_bytes32(),
            9,
            "shapetoken chat b",
            Some(channel),
            None,
            T0 + 400,
        )
        .await;
        let mixed = Some(vec![9, KIND_PAGE_REVISION as i32]);
        let all = run(pool, &query(community, "shapetoken", mixed.clone())).await;
        assert_eq!(all.len(), 5, "{policy:?}: 2 messages + 3 page heads");
        let mut walked = Vec::new();
        for page in 1..=3 {
            let mut q = query(community, "shapetoken", mixed.clone());
            q.per_page = 2;
            q.page = page;
            walked.extend(hit_ids(&run(pool, &q).await));
        }
        assert_eq!(walked, hit_ids(&all), "{policy:?}: mixed paging is stable");
        let mut kinds: Vec<i32> = all.iter().map(|h| h.kind).collect();
        kinds.sort();
        assert_eq!(kinds, vec![9, 9, 52000, 52000, 52000], "{policy:?}");
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn filters_modes_and_paging_apply_to_page_heads() {
    under_each_policy(filters_modes_and_paging_scenario).await;
}

fn pages_are_opt_in_by_kind<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "optin.example").await;
        let channel = mk_channel(pool, community).await;
        let mut page = Page::new(community, channel);
        let head = page.revise(pool, "optintoken body", T0).await;
        let msg = rand_bytes32();
        insert_event(
            pool,
            community,
            msg,
            rand_bytes32(),
            9,
            "optintoken chat",
            Some(channel),
            None,
            T0 + 1,
        )
        .await;

        // A kindless search keeps its default scope: no page revision, head or
        // not, under either policy.
        let hits = run(pool, &query(community, "optintoken", None)).await;
        assert_eq!(hit_ids(&hits), vec![msg], "{policy:?}: kindless");
        // Explicit non-page kinds likewise.
        let hits = run(pool, &query(community, "optintoken", Some(vec![9, 40002]))).await;
        assert_eq!(hit_ids(&hits), vec![msg], "{policy:?}: explicit chat kinds");
        // Naming the revision kind opts in.
        let hits = run(pool, &query(community, "optintoken", pages_kinds())).await;
        assert_eq!(hit_ids(&hits), vec![head], "{policy:?}: page kind");
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn pages_are_searched_only_when_the_revision_kind_is_named() {
    under_each_policy(pages_are_opt_in_by_kind).await;
}

// -- Non-page search is unchanged ----------------------------------------------------------

/// The statement `buzz_search::search` ran before page awareness, kept verbatim
/// (FullText mode, no exact-profile priority) so the regression test compares
/// against what shipped, not against a re-derivation of the new code.
async fn legacy_search(pool: &PgPool, q: &SearchQuery) -> Vec<(SearchHit, i32)> {
    let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
        "SELECT id, kind, pubkey, channel_id, \
         EXTRACT(EPOCH FROM created_at)::bigint AS created_at_s, \
         ts_rank_cd(search_tsv, search_query.query) AS rank \
         FROM events CROSS JOIN LATERAL (SELECT websearch_to_tsquery('simple', ",
    );
    qb.push_bind(q.q.clone());
    qb.push(") AS query) AS search_query WHERE community_id = ");
    qb.push_bind(*q.community.as_uuid());
    qb.push(" AND deleted_at IS NULL AND search_tsv @@ search_query.query");
    match &q.channel_scope {
        ChannelScope::Any => {}
        ChannelScope::ChannelLessOnly => {
            qb.push(" AND channel_id IS NULL");
        }
        ChannelScope::Channels(ids) => {
            qb.push(" AND channel_id = ANY(");
            qb.push_bind(ids.clone());
            qb.push(")");
        }
        ChannelScope::ChannelsOrChannelLess(ids) => {
            qb.push(" AND (channel_id = ANY(");
            qb.push_bind(ids.clone());
            qb.push(") OR channel_id IS NULL)");
        }
    }
    if let Some(kinds) = &q.kinds {
        qb.push(" AND kind = ANY(");
        qb.push_bind(kinds.clone());
        qb.push(")");
    }
    if let Some(since) = q.since {
        qb.push(" AND created_at >= to_timestamp(");
        qb.push_bind(since);
        qb.push(")");
    }
    if let Some(until) = q.until {
        qb.push(" AND created_at <= to_timestamp(");
        qb.push_bind(until);
        qb.push(")");
    }
    qb.push(" ORDER BY rank DESC, created_at DESC, id LIMIT ");
    qb.push_bind(q.per_page as i64);
    qb.push(" OFFSET ");
    qb.push_bind(((q.page - 1) * q.per_page) as i64);
    let rows = qb.build().fetch_all(pool).await.expect("legacy search");
    rows.into_iter()
        .map(|row| {
            let id: Vec<u8> = row.get("id");
            let pk: Vec<u8> = row.get("pubkey");
            let kind: i32 = row.get("kind");
            (
                SearchHit {
                    event_id: id.try_into().expect("32-byte id"),
                    kind,
                    pubkey: pk.try_into().expect("32-byte pubkey"),
                    channel_id: row.get("channel_id"),
                    created_at: row.get("created_at_s"),
                    rank: row.get("rank"),
                },
                kind,
            )
        })
        .collect()
}

fn non_page_search_is_identical<'a>(
    pool: &'a PgPool,
    policy: FtsPolicy,
) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
    Box::pin(async move {
        let community = mk_community(pool, "regress.example").await;
        let channel = mk_channel(pool, community).await;
        let other = mk_channel(pool, community).await;

        // Messages across kinds, channels and ranks (repeat counts vary rank).
        for i in 0..24_i64 {
            let kind = [9, 40002, 45001, 45003, 0][(i % 5) as usize];
            let repeats = "regresstoken ".repeat(1 + (i % 4) as usize);
            insert_event(
                pool,
                community,
                rand_bytes32(),
                rand_bytes32(),
                kind,
                &format!("{repeats}filler {i}"),
                Some(if i % 3 == 0 { other } else { channel }),
                None,
                T0 + i * 7,
            )
            .await;
        }
        // Pages with the same token: heads, superseded revisions, suggestions.
        let mut page = Page::new(community, channel);
        page.revise(pool, "regresstoken regresstoken old", T0 + 1)
            .await;
        page.revise(pool, "regresstoken head", T0 + 2).await;
        page.side_event(pool, KIND_PAGE_SUGGESTION, "regresstoken suggested", T0 + 3)
            .await;

        let non_page = |hits: Vec<(SearchHit, i32)>| -> Vec<SearchHit> {
            hits.into_iter()
                .filter(|(_, kind)| !(52000..=52002).contains(kind))
                .map(|(hit, _)| hit)
                .collect()
        };
        let same = |a: &[SearchHit], b: &[SearchHit], label: &str| {
            assert_eq!(a.len(), b.len(), "{policy:?} {label}: length");
            for (x, y) in a.iter().zip(b) {
                assert_eq!(x.event_id, y.event_id, "{policy:?} {label}: order");
                assert_eq!(x.kind, y.kind, "{policy:?} {label}");
                assert_eq!(x.pubkey, y.pubkey, "{policy:?} {label}");
                assert_eq!(x.channel_id, y.channel_id, "{policy:?} {label}");
                assert_eq!(x.created_at, y.created_at, "{policy:?} {label}");
                assert_eq!(x.rank, y.rank, "{policy:?} {label}: rank");
            }
        };

        // (label, query, exact): `exact` shapes name only non-page kinds, so the
        // pre-change statement must agree row for row, rank for rank, page for
        // page. Kindless shapes compare to the pre-change result minus page
        // rows (the only intended difference), on one page that holds them all.
        let mut shapes: Vec<(&str, SearchQuery, bool)> = Vec::new();
        shapes.push(("kindless", query(community, "regresstoken", None), false));
        shapes.push((
            "explicit chat kinds",
            query(
                community,
                "regresstoken",
                Some(vec![9, 40002, 45001, 45003]),
            ),
            true,
        ));
        shapes.push((
            "profile kind",
            query(community, "regresstoken", Some(vec![0])),
            true,
        ));
        let mut scoped = query(
            community,
            "regresstoken",
            Some(vec![9, 40002, 45001, 45003]),
        );
        scoped.channel_scope = ChannelScope::Channels(vec![channel]);
        shapes.push(("channel scope", scoped, true));
        let mut windowed = query(community, "regresstoken", None);
        windowed.since = Some(T0 + 30);
        windowed.until = Some(T0 + 120);
        shapes.push(("kindless since/until", windowed, false));
        let mut paged = query(
            community,
            "regresstoken",
            Some(vec![9, 40002, 45001, 45003]),
        );
        paged.per_page = 5;
        paged.page = 2;
        shapes.push(("chat kinds, page 2 of 5", paged, true));

        for (label, q, exact) in shapes {
            let new = run(pool, &q).await;
            let legacy = legacy_search(pool, &q).await;
            let expected = if exact {
                legacy.into_iter().map(|(hit, _)| hit).collect()
            } else {
                non_page(legacy)
            };
            assert!(
                !expected.is_empty(),
                "{policy:?} {label}: vacuous comparison"
            );
            same(&new, &expected, label);
            assert!(
                new.iter().all(|h| !(52000..=52002).contains(&h.kind)),
                "{policy:?} {label}: page rows leaked into a non-page search"
            );
        }
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn non_page_search_results_are_unchanged() {
    under_each_policy(non_page_search_is_identical).await;
}
