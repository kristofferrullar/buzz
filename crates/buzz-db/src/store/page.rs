//! Pages head index (NIP-PG).
//!
//! The `pages` table is a projection of the append-only `PAGE_REVISION` events
//! (kind 52000): one row per page naming its current head revision, title and
//! timestamps. A page is identified by the `(h, d)` pair — the channel it lives
//! in and its page id — so the same page id in two channels is two pages. The
//! primary key is `(community_id, channel_id, page_id)` and every query here is
//! community-scoped.
//!
//! ## Head advance (NIP-PG rule 3)
//!
//! A page's first revision (no `prev`) creates its row; every later revision
//! advances the head with a compare-and-swap on `prev`. A failed swap is the
//! typed [`PageHeadError::Conflict`] carrying the current head, never a generic
//! database error, so the relay can answer `conflict:`. The advance functions
//! take the caller's transaction so the relay can store the event and move the
//! head as one atomic unit; the caller owns commit and rollback.
//!
//! ## Rebuild by replay (NIP-PG "Rebuild Invariant")
//!
//! The index holds nothing the event log does not. [`rebuild_pages_for_channel_in_transaction`]
//! recomputes rows from stored revision events alone using [`resolve_page`], the
//! single definition of the projection:
//!
//! - tips are revisions no other revision names as `prev`;
//! - the head is the tip with the greatest `created_at`, ties broken by the
//!   lowest event id; if that head is soft-deleted the head is its `prev`, and
//!   so on up the chain; a page with no live head has no row;
//! - `created_by`/`created_at` come from the first (root) revision, and
//!   `updated_by`/`updated_at`/`title` from the head revision, so they are event
//!   data and never wall-clock time;
//! - `revision_count` counts every valid stored revision, soft-deleted or not.
//!
//! A page deleted through [`soft_delete_page_in_transaction`] has its events
//! soft-deleted in the same transaction, so a rebuild drops it instead of
//! resurrecting it.
//!
//! ## Concurrency
//!
//! Writers take a shared transaction-scoped advisory lock on the page's channel;
//! a rebuild takes it exclusively. A rebuild therefore never publishes a head
//! computed from a snapshot that a concurrent writer has since moved past.

use std::collections::{BTreeSet, HashMap, HashSet};

use buzz_core::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION};
use buzz_core::page::{MAX_PAGE_TITLE_BYTES, TAG_PAGE_ID, TAG_PREV, TAG_TITLE};
use buzz_core::CommunityId;
use buzz_datastore_tracing::datastore_span;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::error::{DbError, Result};
use crate::Db;

/// Channel (NIP-29 group) tag name shared by all page events.
const TAG_CHANNEL: &str = "h";

/// Default number of pages returned by [`list_pages_for_channels`].
pub const PAGE_LIST_DEFAULT_LIMIT: u32 = 50;
/// Hard ceiling on the page size of [`list_pages_for_channels`].
pub const PAGE_LIST_MAX_LIMIT: u32 = 200;
/// Channels bound into one listing query; larger sets are queried in chunks and
/// merged, so the size of a single statement stays bounded.
const LIST_CHANNEL_CHUNK: usize = 1000;
/// Most revisions of one page a replay will load. A page beyond this fails the
/// rebuild loudly rather than being silently truncated.
pub const MAX_REPLAY_REVISIONS_PER_PAGE: usize = 100_000;

/// First key of the two-int advisory lock that fences page writers against a
/// rebuild (ASCII `PGIX`). The second key hashes the community and channel. The
/// two-int form cannot collide with the single-bigint locks other stores take.
/// It is taken with plain statements rather than `observe_advisory_lock`: that
/// metric's `LockType` vocabulary is a closed, tested label set shared with
/// upstream, and extending it is not worth a fork-wide edit for one lock.
const PAGE_INDEX_LOCK_NAMESPACE: i32 = 0x5047_4958;

macro_rules! page_columns {
    () => {
        "channel_id, page_id, head_event_id, title, created_by, created_at, updated_by, \
         updated_at, revision_count, deleted_at"
    };
}

// -- Revision metadata ---------------------------------------------------------

/// Why an event cannot be indexed as a page revision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PageRevisionError {
    /// The event is not a `PAGE_REVISION` (kind 52000).
    #[error("not a page revision event (kind {0})")]
    NotARevision(u32),
    /// A required tag is missing, or a single-valued tag appears more than once.
    #[error("page revision must carry exactly one `{0}` tag")]
    TagCardinality(&'static str),
    /// A tag is present but its value is unusable.
    #[error("page revision tag `{tag}` is malformed: {reason}")]
    MalformedTag {
        /// Tag name.
        tag: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The event timestamp does not fit a database timestamp.
    #[error("page revision created_at {0} is out of range")]
    Timestamp(i64),
}

/// The fields of a `PAGE_REVISION` event the head index projects.
///
/// Built from a live event with [`PageRevisionMeta::from_event`] and from stored
/// rows during replay, through one parser, so the live index and a rebuild read
/// events identically. A revision carries exactly one `h`, one `d` and one
/// `title` tag and at most one `prev` tag; page and channel ids must be
/// canonical lowercase hyphenated UUIDs and `prev` 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRevisionMeta {
    /// Event id of this revision.
    pub event_id: [u8; 32],
    /// Author public key.
    pub author: [u8; 32],
    /// The event's own `created_at`.
    pub created_at: DateTime<Utc>,
    /// Channel (`h` tag) the page lives in.
    pub channel_id: Uuid,
    /// Page id (`d` tag).
    pub page_id: Uuid,
    /// Revision this one was based on; `None` for a page's first revision.
    pub prev: Option<[u8; 32]>,
    /// Page title (`title` tag).
    pub title: String,
}

impl PageRevisionMeta {
    /// Extract the projection fields from a signed `PAGE_REVISION` event.
    pub fn from_event(event: &nostr::Event) -> std::result::Result<Self, PageRevisionError> {
        let kind = u32::from(event.kind.as_u16());
        if kind != KIND_PAGE_REVISION {
            return Err(PageRevisionError::NotARevision(kind));
        }
        let secs = i64::try_from(event.created_at.as_secs())
            .map_err(|_| PageRevisionError::Timestamp(i64::MAX))?;
        let created_at =
            DateTime::from_timestamp(secs, 0).ok_or(PageRevisionError::Timestamp(secs))?;
        let tags: Vec<Vec<String>> = event
            .tags
            .iter()
            .map(|tag| tag.as_slice().to_vec())
            .collect();
        parse_revision(
            *event.id.as_bytes(),
            event.pubkey.to_bytes(),
            created_at,
            &tags,
        )
    }
}

/// The single value of tag `name`, `None` if absent, an error if repeated or
/// valueless.
fn single_tag<'a>(
    tags: &'a [Vec<String>],
    name: &'static str,
) -> std::result::Result<Option<&'a str>, PageRevisionError> {
    let mut found = tags
        .iter()
        .filter(|tag| tag.first().map(String::as_str) == Some(name));
    let Some(tag) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(PageRevisionError::TagCardinality(name));
    }
    tag.get(1)
        .map(String::as_str)
        .map(Some)
        .ok_or(PageRevisionError::MalformedTag {
            tag: name,
            reason: "missing value",
        })
}

fn canonical_uuid(tag: &'static str, value: &str) -> std::result::Result<Uuid, PageRevisionError> {
    let parsed = Uuid::parse_str(value).map_err(|_| PageRevisionError::MalformedTag {
        tag,
        reason: "not a UUID",
    })?;
    if parsed.hyphenated().to_string() != value {
        return Err(PageRevisionError::MalformedTag {
            tag,
            reason: "not a canonical lowercase UUID",
        });
    }
    Ok(parsed)
}

fn lowercase_hex_32(
    tag: &'static str,
    value: &str,
) -> std::result::Result<[u8; 32], PageRevisionError> {
    let malformed = |reason| PageRevisionError::MalformedTag { tag, reason };
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(malformed("not 64 lowercase hex characters"));
    }
    let mut out = [0_u8; 32];
    hex::decode_to_slice(value, &mut out).map_err(|_| malformed("not hex"))?;
    Ok(out)
}

fn parse_revision(
    event_id: [u8; 32],
    author: [u8; 32],
    created_at: DateTime<Utc>,
    tags: &[Vec<String>],
) -> std::result::Result<PageRevisionMeta, PageRevisionError> {
    let channel = single_tag(tags, TAG_CHANNEL)?.ok_or(PageRevisionError::TagCardinality("h"))?;
    let page = single_tag(tags, TAG_PAGE_ID)?.ok_or(PageRevisionError::TagCardinality("d"))?;
    let title = single_tag(tags, TAG_TITLE)?.ok_or(PageRevisionError::TagCardinality(TAG_TITLE))?;
    let prev = single_tag(tags, TAG_PREV)?
        .map(|value| lowercase_hex_32(TAG_PREV, value))
        .transpose()?;
    if title.trim().is_empty() {
        return Err(PageRevisionError::MalformedTag {
            tag: TAG_TITLE,
            reason: "blank",
        });
    }
    if title.len() > MAX_PAGE_TITLE_BYTES {
        return Err(PageRevisionError::MalformedTag {
            tag: TAG_TITLE,
            reason: "longer than 256 bytes",
        });
    }
    Ok(PageRevisionMeta {
        event_id,
        author,
        created_at,
        channel_id: canonical_uuid("h", channel)?,
        page_id: canonical_uuid("d", page)?,
        prev,
        title: title.to_owned(),
    })
}

// -- Records, errors -----------------------------------------------------------

/// One row of the page head index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRecord {
    /// Channel the page lives in.
    pub channel_id: Uuid,
    /// Page id, unique within the channel.
    pub page_id: Uuid,
    /// Event id of the head revision.
    pub head_event_id: [u8; 32],
    /// Title of the head revision.
    pub title: String,
    /// Author of the first revision.
    pub created_by: [u8; 32],
    /// Timestamp of the first revision.
    pub created_at: DateTime<Utc>,
    /// Author of the head revision.
    pub updated_by: [u8; 32],
    /// Timestamp of the head revision.
    pub updated_at: DateTime<Utc>,
    /// Revisions accepted into the page's log; soft-deleted ones still count.
    pub revision_count: i32,
    /// Set when the page has been soft-deleted.
    pub deleted_at: Option<DateTime<Utc>>,
}

fn bytes32(row: &PgRow, column: &str) -> Result<[u8; 32]> {
    let raw: Vec<u8> = row.try_get(column)?;
    <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| {
        DbError::InvalidData(format!(
            "pages.{column} holds {} bytes, expected 32",
            raw.len()
        ))
    })
}

fn row_to_record(row: &PgRow) -> Result<PageRecord> {
    Ok(PageRecord {
        channel_id: row.try_get("channel_id")?,
        page_id: row.try_get("page_id")?,
        head_event_id: bytes32(row, "head_event_id")?,
        title: row.try_get("title")?,
        created_by: bytes32(row, "created_by")?,
        created_at: row.try_get("created_at")?,
        updated_by: bytes32(row, "updated_by")?,
        updated_at: row.try_get("updated_at")?,
        revision_count: row.try_get("revision_count")?,
        deleted_at: row.try_get("deleted_at")?,
    })
}

/// Why a head create or advance did not happen.
#[derive(Debug, thiserror::Error)]
pub enum PageHeadError {
    /// NIP-PG rule 3: the revision's `prev` does not match the page's head (or a
    /// first revision names a page that already exists, or a later revision
    /// names one that does not). Nothing was written; callers answer `conflict:`.
    #[error("page head conflict (current head: {})", current_head_hex(.current_head))]
    Conflict {
        /// The page's current head, `None` when the page does not exist.
        current_head: Option<[u8; 32]>,
    },
    /// The page was soft-deleted and accepts no further revisions.
    #[error("page is deleted")]
    Deleted,
    /// A database failure, including malformed caller input.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<sqlx::Error> for PageHeadError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(DbError::Sqlx(error))
    }
}

fn current_head_hex(head: &Option<[u8; 32]>) -> String {
    head.map_or_else(|| "none".to_owned(), hex::encode)
}

// -- Locking -------------------------------------------------------------------

#[derive(Clone, Copy)]
enum PageLock {
    /// Held by head writers; compatible with each other.
    Shared,
    /// Held by a rebuild; excludes every writer of the channel's pages.
    Exclusive,
}

async fn lock_channel_pages(
    conn: &mut PgConnection,
    community: CommunityId,
    channel_id: Uuid,
    mode: PageLock,
) -> std::result::Result<(), sqlx::Error> {
    let sql = match mode {
        PageLock::Shared => "SELECT pg_advisory_xact_lock_shared($1, hashtext($2))",
        PageLock::Exclusive => "SELECT pg_advisory_xact_lock($1, hashtext($2))",
    };
    sqlx::query(sql)
        .bind(PAGE_INDEX_LOCK_NAMESPACE)
        .bind(format!("{}:{channel_id}", community.as_uuid()))
        .execute(conn)
        .await
        .map(|_| ())
}

// -- Head create / advance -----------------------------------------------------

/// Record a `PAGE_REVISION` in the head index inside the caller's transaction.
///
/// Dispatches on `prev`: a first revision creates the page
/// ([`create_page_head_in_transaction`]), any other advances it
/// ([`advance_page_head_in_transaction`]). The caller stores the event in the
/// same transaction and commits or rolls back both together.
pub async fn record_page_revision_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    revision: &PageRevisionMeta,
) -> std::result::Result<PageRecord, PageHeadError> {
    if revision.prev.is_none() {
        create_page_head_in_transaction(tx, community, revision).await
    } else {
        advance_page_head_in_transaction(tx, community, revision).await
    }
}

/// Create a page from its first revision (no `prev`).
///
/// Fails with [`PageHeadError::Conflict`] carrying the existing head when the
/// `(channel, page)` identity already exists in `community`, and with
/// [`PageHeadError::Deleted`] when it exists as a tombstone. A revision that
/// carries a `prev` is a caller bug and is rejected as invalid data.
pub async fn create_page_head_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    revision: &PageRevisionMeta,
) -> std::result::Result<PageRecord, PageHeadError> {
    if revision.prev.is_some() {
        return Err(DbError::InvalidData(
            "create_page_head requires a revision without a prev tag".to_owned(),
        )
        .into());
    }
    lock_channel_pages(
        tx.as_mut(),
        community,
        revision.channel_id,
        PageLock::Shared,
    )
    .await?;
    let row = sqlx::query(concat!(
        "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
         created_by, created_at, updated_by, updated_at, revision_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $6, $7, 1) \
         ON CONFLICT (community_id, channel_id, page_id) DO NOTHING \
         RETURNING ",
        page_columns!()
    ))
    .bind(community.as_uuid())
    .bind(revision.channel_id)
    .bind(revision.page_id)
    .bind(revision.event_id.as_slice())
    .bind(&revision.title)
    .bind(revision.author.as_slice())
    .bind(revision.created_at)
    .fetch_optional(tx.as_mut())
    .await?;
    match row {
        Some(row) => Ok(row_to_record(&row)?),
        None => Err(conflict_or_deleted(
            tx.as_mut(),
            community,
            revision.channel_id,
            revision.page_id,
        )
        .await?),
    }
}

/// Advance a page's head to `revision`, compare-and-swap on its `prev` tag.
///
/// The swap succeeds only if the page exists, is not deleted, and its current
/// head equals `revision.prev` (NIP-PG rule 3). Otherwise nothing changes and
/// the result is [`PageHeadError::Conflict`] carrying the current head
/// (`None` when the page does not exist) or [`PageHeadError::Deleted`]. Two
/// writers racing on the same head serialize on the row lock: the second sees
/// the new head when its predicate is re-evaluated and gets the conflict.
///
/// A revision without `prev` is a caller bug and is rejected as invalid data.
pub async fn advance_page_head_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    revision: &PageRevisionMeta,
) -> std::result::Result<PageRecord, PageHeadError> {
    let Some(prev) = revision.prev else {
        return Err(DbError::InvalidData(
            "advance_page_head requires a revision with a prev tag".to_owned(),
        )
        .into());
    };
    lock_channel_pages(
        tx.as_mut(),
        community,
        revision.channel_id,
        PageLock::Shared,
    )
    .await?;
    let row = sqlx::query(concat!(
        "UPDATE pages SET head_event_id = $4, title = $5, updated_by = $6, updated_at = $7, \
         revision_count = revision_count + 1 \
         WHERE community_id = $1 AND channel_id = $2 AND page_id = $3 \
           AND head_event_id = $8 AND deleted_at IS NULL \
         RETURNING ",
        page_columns!()
    ))
    .bind(community.as_uuid())
    .bind(revision.channel_id)
    .bind(revision.page_id)
    .bind(revision.event_id.as_slice())
    .bind(&revision.title)
    .bind(revision.author.as_slice())
    .bind(revision.created_at)
    .bind(prev.as_slice())
    .fetch_optional(tx.as_mut())
    .await?;
    match row {
        Some(row) => Ok(row_to_record(&row)?),
        None => Err(conflict_or_deleted(
            tx.as_mut(),
            community,
            revision.channel_id,
            revision.page_id,
        )
        .await?),
    }
}

/// Classify a failed create or swap from the page's current state.
async fn conflict_or_deleted(
    conn: &mut PgConnection,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> std::result::Result<PageHeadError, sqlx::Error> {
    let row = sqlx::query(
        "SELECT head_event_id, deleted_at FROM pages \
         WHERE community_id = $1 AND channel_id = $2 AND page_id = $3",
    )
    .bind(community.as_uuid())
    .bind(channel_id)
    .bind(page_id)
    .fetch_optional(conn)
    .await?;
    let Some(row) = row else {
        return Ok(PageHeadError::Conflict { current_head: None });
    };
    let deleted_at: Option<DateTime<Utc>> = row.try_get("deleted_at")?;
    if deleted_at.is_some() {
        return Ok(PageHeadError::Deleted);
    }
    match bytes32(&row, "head_event_id") {
        Ok(head) => Ok(PageHeadError::Conflict {
            current_head: Some(head),
        }),
        Err(error) => Ok(PageHeadError::Db(error)),
    }
}

// -- Reads ---------------------------------------------------------------------

async fn get_page_on(
    conn: &mut PgConnection,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
    include_deleted: bool,
) -> Result<Option<PageRecord>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        page_columns!(),
        " FROM pages WHERE community_id = $1 AND channel_id = $2 AND page_id = $3 \
         AND ($4 OR deleted_at IS NULL)"
    ))
    .bind(community.as_uuid())
    .bind(channel_id)
    .bind(page_id)
    .bind(include_deleted)
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(row_to_record).transpose()
}

/// Look up a live page by its `(channel, page)` identity in `community`.
///
/// Soft-deleted pages are not returned; use [`get_page_including_deleted`].
pub async fn get_page(
    pool: &PgPool,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<Option<PageRecord>> {
    let mut conn = crate::observability::acquire_writer(
        pool,
        crate::observability::WriterOperation::Authorization,
    )
    .await?;
    get_page_on(&mut conn, community, channel_id, page_id, false).await
}

/// Look up a page by identity, including a soft-deleted tombstone.
pub async fn get_page_including_deleted(
    pool: &PgPool,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<Option<PageRecord>> {
    let mut conn = crate::observability::acquire_writer(
        pool,
        crate::observability::WriterOperation::Authorization,
    )
    .await?;
    get_page_on(&mut conn, community, channel_id, page_id, true).await
}

/// Look up a live page inside the caller's transaction, so a head read and a
/// following advance observe one transaction.
pub async fn get_page_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<Option<PageRecord>> {
    get_page_on(tx.as_mut(), community, channel_id, page_id, false).await
}

/// Keyset position in the library listing: the last page of the previous batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCursor {
    /// `updated_at` of the last page returned.
    pub updated_at: DateTime<Utc>,
    /// Channel of the last page returned.
    pub channel_id: Uuid,
    /// Page id of the last page returned.
    pub page_id: Uuid,
}

impl PageCursor {
    fn of(record: &PageRecord) -> Self {
        Self {
            updated_at: record.updated_at,
            channel_id: record.channel_id,
            page_id: record.page_id,
        }
    }

    fn key(&self) -> (DateTime<Utc>, Uuid, Uuid) {
        (self.updated_at, self.channel_id, self.page_id)
    }
}

/// One batch of the library listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageListing {
    /// Pages, newest-updated first.
    pub pages: Vec<PageRecord>,
    /// Pass back as `after` to fetch the next batch; `None` on the last batch.
    pub next_cursor: Option<PageCursor>,
}

/// List live pages in the given channels, newest-updated first.
///
/// `channel_ids` is the set of channels the viewer can read (the relay decides
/// that; this function only scopes to `community` and the given channels).
/// Pages of soft-deleted channels are excluded. At most `limit` pages are
/// returned (clamped to `1..=`[`PAGE_LIST_MAX_LIMIT`]); pass the returned
/// `next_cursor` as `after` for the following batch. Ordering is
/// `(updated_at, channel_id, page_id)` descending, a total order, so batches
/// neither repeat nor skip pages that are not modified between calls. Channel
/// sets larger than one statement's budget are queried in chunks and merged.
pub async fn list_pages_for_channels(
    pool: &PgPool,
    community: CommunityId,
    channel_ids: &[Uuid],
    limit: u32,
    after: Option<&PageCursor>,
) -> Result<PageListing> {
    let limit = limit.clamp(1, PAGE_LIST_MAX_LIMIT) as usize;
    let channels: Vec<Uuid> = channel_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut conn = crate::observability::acquire_writer(
        pool,
        crate::observability::WriterOperation::SubscriptionHistory,
    )
    .await?;
    let mut merged: Vec<PageRecord> = Vec::new();
    for chunk in channels.chunks(LIST_CHANNEL_CHUNK) {
        let rows = sqlx::query(concat!(
            "SELECT ",
            page_columns!(),
            " FROM pages p \
             WHERE p.community_id = $1 AND p.channel_id = ANY($2) AND p.deleted_at IS NULL \
               AND ($3::timestamptz IS NULL \
                    OR (p.updated_at, p.channel_id, p.page_id) < ($3, $4::uuid, $5::uuid)) \
               AND EXISTS (SELECT 1 FROM channels c \
                           WHERE c.community_id = p.community_id AND c.id = p.channel_id \
                             AND c.deleted_at IS NULL) \
             ORDER BY p.updated_at DESC, p.channel_id DESC, p.page_id DESC \
             LIMIT $6"
        ))
        .bind(community.as_uuid())
        .bind(chunk)
        .bind(after.map(|c| c.updated_at))
        .bind(after.map(|c| c.channel_id))
        .bind(after.map(|c| c.page_id))
        .bind(i64::try_from(limit + 1).unwrap_or(i64::MAX))
        .fetch_all(&mut *conn)
        .await?;
        for row in &rows {
            merged.push(row_to_record(row)?);
        }
    }
    merged.sort_by_key(|record| std::cmp::Reverse(PageCursor::of(record).key()));
    let has_more = merged.len() > limit;
    merged.truncate(limit);
    let next_cursor = if has_more {
        merged.last().map(PageCursor::of)
    } else {
        None
    };
    Ok(PageListing {
        pages: merged,
        next_cursor,
    })
}

// -- Soft delete ---------------------------------------------------------------

/// Soft-delete a page inside the caller's transaction.
///
/// Tombstones the index row and soft-deletes the page's revision, suggestion and
/// resolution events in the same transaction, so the page vanishes from every
/// read and a rebuild by replay agrees (it finds no live revision). Returns
/// `false` without touching anything when there is no live page with this
/// identity in `community`.
pub async fn soft_delete_page_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<bool> {
    lock_channel_pages(tx.as_mut(), community, channel_id, PageLock::Shared).await?;
    let tombstoned = sqlx::query(
        "UPDATE pages SET deleted_at = NOW() \
         WHERE community_id = $1 AND channel_id = $2 AND page_id = $3 AND deleted_at IS NULL",
    )
    .bind(community.as_uuid())
    .bind(channel_id)
    .bind(page_id)
    .execute(tx.as_mut())
    .await?
    .rows_affected()
        > 0;
    if !tombstoned {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE events SET deleted_at = NOW() \
         WHERE community_id = $1 AND kind = ANY($2) AND deleted_at IS NULL \
           AND tags @> $3::jsonb AND tags @> $4::jsonb",
    )
    .bind(community.as_uuid())
    .bind(vec![
        KIND_PAGE_REVISION as i32,
        KIND_PAGE_SUGGESTION as i32,
        KIND_PAGE_SUGGESTION_RESOLUTION as i32,
    ])
    .bind(tag_containment(TAG_CHANNEL, channel_id))
    .bind(tag_containment(TAG_PAGE_ID, page_id))
    .execute(tx.as_mut())
    .await?;
    Ok(true)
}

/// `[[name, id]]`, the JSONB containment probe for an event tag.
fn tag_containment(name: &str, id: Uuid) -> serde_json::Value {
    serde_json::json!([[name, id.to_string()]])
}

// -- Replay --------------------------------------------------------------------

/// A stored revision as replay sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RevisionNode {
    pub(crate) meta: PageRevisionMeta,
    /// The event has been soft-deleted.
    pub(crate) deleted: bool,
}

/// The index row a page's stored revisions imply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedPage {
    pub(crate) head: PageRevisionMeta,
    pub(crate) created_by: [u8; 32],
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) revision_count: i32,
}

/// Compute a page's index row from its stored revisions alone.
///
/// This is the one definition of the NIP-PG head rules: the head is the tip
/// (a revision no other revision names as `prev`) with the greatest `created_at`,
/// ties broken by the lowest event id; a soft-deleted head is replaced by its
/// `prev`, repeatedly. Returns `None` when no live head remains.
pub(crate) fn resolve_page(revisions: &[RevisionNode]) -> Option<ResolvedPage> {
    let by_id: HashMap<[u8; 32], &RevisionNode> = revisions
        .iter()
        .map(|node| (node.meta.event_id, node))
        .collect();
    let referenced: HashSet<[u8; 32]> =
        revisions.iter().filter_map(|node| node.meta.prev).collect();
    let mut head = revisions
        .iter()
        .filter(|node| !referenced.contains(&node.meta.event_id))
        .max_by(|a, b| {
            a.meta
                .created_at
                .cmp(&b.meta.created_at)
                .then_with(|| b.meta.event_id.cmp(&a.meta.event_id))
        })?;
    // The chain is a hash chain and cannot loop; the bound only guards corrupt
    // input from spinning.
    for _ in 0..=revisions.len() {
        if !head.deleted {
            let root = revisions
                .iter()
                .filter(|node| node.meta.prev.is_none_or(|prev| !by_id.contains_key(&prev)))
                .min_by(|a, b| {
                    a.meta
                        .created_at
                        .cmp(&b.meta.created_at)
                        .then_with(|| a.meta.event_id.cmp(&b.meta.event_id))
                })?;
            return Some(ResolvedPage {
                head: head.meta.clone(),
                created_by: root.meta.author,
                created_at: root.meta.created_at,
                revision_count: i32::try_from(revisions.len()).unwrap_or(i32::MAX),
            });
        }
        head = by_id.get(&head.meta.prev?)?;
    }
    None
}

/// Result of replaying one or more pages from their events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageRebuildReport {
    /// Pages whose index row was written from live revisions.
    pub pages_upserted: usize,
    /// Index rows removed because the page has no live revision left.
    pub pages_removed: usize,
    /// Stored page events skipped because they are not valid revisions (an event
    /// naming several page ids is counted once per page id).
    pub malformed_events: usize,
}

impl PageRebuildReport {
    fn absorb(&mut self, other: Self) {
        self.pages_upserted += other.pages_upserted;
        self.pages_removed += other.pages_removed;
        self.malformed_events += other.malformed_events;
    }
}

/// Parse one stored event row as a revision, `None` if it is not a valid one.
fn revision_node(
    id: &[u8],
    author: &[u8],
    created_at: DateTime<Utc>,
    tags: serde_json::Value,
    deleted: bool,
) -> Option<RevisionNode> {
    let id = <[u8; 32]>::try_from(id).ok()?;
    let author = <[u8; 32]>::try_from(author).ok()?;
    let tags: Vec<Vec<String>> = serde_json::from_value(tags).ok()?;
    let meta = parse_revision(id, author, created_at, &tags).ok()?;
    Some(RevisionNode { meta, deleted })
}

/// Load a page's stored revisions (soft-deleted ones included) from `events`.
async fn load_revisions(
    conn: &mut PgConnection,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<(Vec<RevisionNode>, usize)> {
    let cap = i64::try_from(MAX_REPLAY_REVISIONS_PER_PAGE).unwrap_or(i64::MAX);
    let rows = sqlx::query(
        "SELECT id, pubkey, created_at, tags, deleted_at IS NOT NULL AS is_deleted \
         FROM events \
         WHERE community_id = $1 AND kind = $2 AND tags @> $3::jsonb AND tags @> $4::jsonb \
         ORDER BY created_at, id \
         LIMIT $5",
    )
    .bind(community.as_uuid())
    .bind(KIND_PAGE_REVISION as i32)
    .bind(tag_containment(TAG_CHANNEL, channel_id))
    .bind(tag_containment(TAG_PAGE_ID, page_id))
    .bind(cap + 1)
    .fetch_all(conn)
    .await?;
    if rows.len() as i64 > cap {
        return Err(DbError::InvalidData(format!(
            "page {page_id} in channel {channel_id} has more than {MAX_REPLAY_REVISIONS_PER_PAGE} \
             revisions; refusing a partial replay"
        )));
    }
    let mut nodes = Vec::with_capacity(rows.len());
    let mut malformed = 0;
    for row in &rows {
        // Failing to read a column is a database fault and propagates; only an
        // event whose content is not a valid revision is skipped and counted.
        let id: Vec<u8> = row.try_get("id")?;
        let author: Vec<u8> = row.try_get("pubkey")?;
        let created_at: DateTime<Utc> = row.try_get("created_at")?;
        let tags: serde_json::Value = row.try_get("tags")?;
        let deleted: bool = row.try_get("is_deleted")?;
        match revision_node(&id, &author, created_at, tags, deleted) {
            Some(node) if node.meta.channel_id == channel_id && node.meta.page_id == page_id => {
                nodes.push(node);
            }
            _ => malformed += 1,
        }
    }
    Ok((nodes, malformed))
}

/// Recompute one page's row from its events and write it. The caller holds the
/// exclusive channel lock.
async fn reproject_locked(
    conn: &mut PgConnection,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<(Option<PageRecord>, usize)> {
    let (revisions, malformed) = load_revisions(conn, community, channel_id, page_id).await?;
    let Some(resolved) = resolve_page(&revisions) else {
        sqlx::query(
            "DELETE FROM pages WHERE community_id = $1 AND channel_id = $2 AND page_id = $3",
        )
        .bind(community.as_uuid())
        .bind(channel_id)
        .bind(page_id)
        .execute(conn)
        .await?;
        return Ok((None, malformed));
    };
    let row = sqlx::query(concat!(
        "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
         created_by, created_at, updated_by, updated_at, revision_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (community_id, channel_id, page_id) DO UPDATE SET \
             head_event_id = EXCLUDED.head_event_id, title = EXCLUDED.title, \
             created_by = EXCLUDED.created_by, created_at = EXCLUDED.created_at, \
             updated_by = EXCLUDED.updated_by, updated_at = EXCLUDED.updated_at, \
             revision_count = EXCLUDED.revision_count, deleted_at = NULL \
         RETURNING ",
        page_columns!()
    ))
    .bind(community.as_uuid())
    .bind(channel_id)
    .bind(page_id)
    .bind(resolved.head.event_id.as_slice())
    .bind(&resolved.head.title)
    .bind(resolved.created_by.as_slice())
    .bind(resolved.created_at)
    .bind(resolved.head.author.as_slice())
    .bind(resolved.head.created_at)
    .bind(resolved.revision_count)
    .fetch_one(conn)
    .await?;
    Ok((Some(row_to_record(&row)?), malformed))
}

/// Recompute one page's index row from its stored events, repairing the row
/// after an event was soft-deleted (for example the head revision) or after
/// drift. Writes the row, or removes it when no live revision remains, and
/// returns the resulting row.
///
/// Takes the channel's exclusive page lock for the rest of the transaction.
pub async fn reproject_page_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<Option<PageRecord>> {
    lock_channel_pages(tx.as_mut(), community, channel_id, PageLock::Exclusive).await?;
    Ok(
        reproject_locked(tx.as_mut(), community, channel_id, page_id)
            .await?
            .0,
    )
}

/// Rebuild every page of one channel from its stored `PAGE_REVISION` events.
///
/// The result equals the live index for every live page. Rows with no live
/// revision behind them (orphans, tombstones) are removed; a tombstone whose
/// events were not soft-deleted is resurrected, which is why pages must be
/// deleted through [`soft_delete_page_in_transaction`]. Takes the channel's
/// exclusive page lock for the rest of the transaction.
pub async fn rebuild_pages_for_channel_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    channel_id: Uuid,
) -> Result<PageRebuildReport> {
    lock_channel_pages(tx.as_mut(), community, channel_id, PageLock::Exclusive).await?;
    let mut report = PageRebuildReport::default();
    let indexed: BTreeSet<Uuid> =
        sqlx::query_scalar("SELECT page_id FROM pages WHERE community_id = $1 AND channel_id = $2")
            .bind(community.as_uuid())
            .bind(channel_id)
            .fetch_all(tx.as_mut())
            .await?
            .into_iter()
            .collect();
    let stored = sqlx::query(
        "SELECT t.elem->>1 AS page_id, count(*) AS events \
         FROM events e CROSS JOIN LATERAL jsonb_array_elements(e.tags) AS t(elem) \
         WHERE e.community_id = $1 AND e.kind = $2 AND e.tags @> $3::jsonb \
           AND jsonb_typeof(t.elem) = 'array' AND t.elem->>0 = 'd' \
         GROUP BY 1",
    )
    .bind(community.as_uuid())
    .bind(KIND_PAGE_REVISION as i32)
    .bind(tag_containment(TAG_CHANNEL, channel_id))
    .fetch_all(tx.as_mut())
    .await?;
    let mut page_ids = indexed.clone();
    for row in &stored {
        let raw: Option<String> = row.try_get("page_id")?;
        let events: i64 = row.try_get("events")?;
        match raw.as_deref().map(|value| canonical_uuid("d", value)) {
            Some(Ok(page_id)) => {
                page_ids.insert(page_id);
            }
            _ => report.malformed_events += usize::try_from(events).unwrap_or(usize::MAX),
        }
    }
    for page_id in page_ids {
        let (record, malformed) =
            reproject_locked(tx.as_mut(), community, channel_id, page_id).await?;
        report.malformed_events += malformed;
        if record.is_some() {
            report.pages_upserted += 1;
        } else if indexed.contains(&page_id) {
            report.pages_removed += 1;
        }
    }
    Ok(report)
}

impl Db {
    /// Look up a live page by `(channel, page)` in `community`. See [`get_page`].
    #[datastore_span(name = "get_page", system = "postgresql")]
    pub async fn get_page(
        &self,
        community: CommunityId,
        channel_id: Uuid,
        page_id: Uuid,
    ) -> Result<Option<PageRecord>> {
        get_page(&self.pool, community, channel_id, page_id).await
    }

    /// List live pages in the given channels, newest-updated first. See
    /// [`list_pages_for_channels`].
    #[datastore_span(name = "list_pages_for_channels", system = "postgresql")]
    pub async fn list_pages_for_channels(
        &self,
        community: CommunityId,
        channel_ids: &[Uuid],
        limit: u32,
        after: Option<&PageCursor>,
    ) -> Result<PageListing> {
        list_pages_for_channels(&self.pool, community, channel_ids, limit, after).await
    }

    /// Rebuild the page index of every channel in `community` from stored
    /// events, one transaction per channel.
    ///
    /// Each channel is rebuilt atomically and the whole run is idempotent: after
    /// a failure part-way, rerunning completes it.
    #[datastore_span(name = "rebuild_pages", system = "postgresql")]
    pub async fn rebuild_pages(&self, community: CommunityId) -> Result<PageRebuildReport> {
        let mut conn = crate::observability::acquire_writer(
            &self.pool,
            crate::observability::WriterOperation::Maintenance,
        )
        .await?;
        let channels: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM channels WHERE community_id = $1 ORDER BY id")
                .bind(community.as_uuid())
                .fetch_all(&mut *conn)
                .await?;
        drop(conn);
        let mut report = PageRebuildReport::default();
        for channel_id in channels {
            let mut tx = self.begin_event_write_transaction().await?;
            let channel_report =
                rebuild_pages_for_channel_in_transaction(&mut tx, community, channel_id).await?;
            tx.commit().await?;
            report.absorb(channel_report);
        }
        Ok(report)
    }
}

#[cfg(test)]
mod parse_tests;

#[cfg(test)]
mod postgres_tests;

#[cfg(test)]
mod resolve_tests;
