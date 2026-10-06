//! Primitives the relay composes to ingest NIP-PG page events atomically.
//!
//! The relay validates and stores a page event inside one transaction from
//! [`crate::Db::begin_event_write_transaction`]. Everything here takes that
//! transaction, so a validation read, the event insert and the head move all
//! observe and commit as one unit, and no step ever checks out a second pool
//! connection while the first is held.
//!
//! - [`lock_page_for_write_in_transaction`] serializes writers of one page so
//!   check-then-write rules (suggestion closure, no-op detection) cannot race.
//!   It also bounds how long a writer may wait, so a stuck transaction cannot
//!   pin request handlers.
//! - [`load_event_in_transaction`] reads a referenced event (`prev`, `base`,
//!   `e`, `suggestion`, `rev`) so its `(h, d)` can be compared with the new
//!   event's.
//! - [`suggestion_state_in_transaction`] reports whether a suggestion is closed.
//! - [`insert_page_event_in_transaction`] stores the event and its mention rows.
//! - [`soft_delete_page_event_in_transaction`] deletes one page event and repairs
//!   the head index in the same transaction.

use buzz_core::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION_RESOLUTION};
use buzz_core::page::{is_page_kind, TAG_PAGE_ID};
use buzz_core::{CommunityId, StoredEvent};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{
    canonical_uuid, load_revisions, lowercase_hex_32, reproject_page_in_transaction, resolve_page,
    row_to_record, PageRecord, PageRevisionError, PageRevisionMeta, MAX_REPLAY_REVISIONS_PER_PAGE,
    TAG_CHANNEL,
};
use crate::error::{DbError, Result};

/// First key of the two-int advisory lock that serializes writers of one page
/// (ASCII `PGPW`). It is distinct from the channel-wide lock that fences writers
/// against a rebuild, and is always taken before it.
const PAGE_WRITE_LOCK_NAMESPACE: i32 = 0x5047_5057;

/// How long a page writer may wait on any lock before the transaction fails.
///
/// The lock timeout applies to the rest of the transaction, covering the page
/// lock, the channel lock and the head row lock. Postgres reports a timeout as
/// SQLSTATE `55P03`; see [`is_lock_timeout`].
pub const PAGE_WRITE_LOCK_TIMEOUT_MS: u32 = 5_000;

/// Parse a canonical lowercase hyphenated UUID tag value.
///
/// Page and channel ids are indexed only in this form (NIP-PG "Tags"), so the
/// relay and a replay must accept exactly the same spellings.
pub fn parse_page_uuid(
    tag: &'static str,
    value: &str,
) -> std::result::Result<Uuid, PageRevisionError> {
    canonical_uuid(tag, value)
}

/// Parse a 64-character lowercase hex event id tag value.
pub fn parse_page_event_id(
    tag: &'static str,
    value: &str,
) -> std::result::Result<[u8; 32], PageRevisionError> {
    lowercase_hex_32(tag, value)
}

/// `true` if `error` is Postgres reporting that a lock wait exceeded
/// [`PAGE_WRITE_LOCK_TIMEOUT_MS`] (SQLSTATE `55P03`, `lock_not_available`).
pub fn is_lock_timeout(error: &DbError) -> bool {
    match error {
        DbError::Sqlx(sqlx::Error::Database(db)) => db.code().as_deref() == Some("55P03"),
        _ => false,
    }
}

/// Serialize writers of the page `(channel_id, page_id)` for the rest of the
/// transaction and bound every later lock wait in it.
///
/// Two writers of the same page queue here, so a rule that reads state and then
/// writes (a suggestion must still be open when it is applied, a revision must
/// differ from the one it replaces) is decided against the state the previous
/// writer committed. Writers of different pages do not wait on each other. A
/// hash collision between two pages only adds waiting, never a wrong answer.
pub async fn lock_page_for_write_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<()> {
    sqlx::query("SELECT set_config('lock_timeout', $1, true)")
        .bind(format!("{PAGE_WRITE_LOCK_TIMEOUT_MS}ms"))
        .execute(tx.as_mut())
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(PAGE_WRITE_LOCK_NAMESPACE)
        .bind(format!("{}:{channel_id}:{page_id}", community.as_uuid()))
        .execute(tx.as_mut())
        .await?;
    Ok(())
}

/// A stored page-kind event as ingest validation reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEventRecord {
    /// Event id.
    pub id: [u8; 32],
    /// Event kind.
    pub kind: u32,
    /// Author public key.
    pub author: [u8; 32],
    /// The `channel_id` the event was stored under.
    pub channel_id: Option<Uuid>,
    /// All tags, each as its string elements.
    pub tags: Vec<Vec<String>>,
    /// Event content.
    pub content: String,
}

impl PageEventRecord {
    /// Value (second element) of the first tag named `name`.
    pub fn tag_value(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|tag| tag.first().map(String::as_str) == Some(name))
            .and_then(|tag| tag.get(1))
            .map(String::as_str)
    }

    /// The `(channel, page)` this event belongs to, `None` if either tag is
    /// missing or not a canonical UUID.
    ///
    /// Reads the `h` and `d` tags, not the stored `channel_id`, because the tags
    /// are what a replay sees.
    pub fn page_identity(&self) -> Option<(Uuid, Uuid)> {
        let channel = canonical_uuid(TAG_CHANNEL, self.tag_value(TAG_CHANNEL)?).ok()?;
        let page = canonical_uuid(TAG_PAGE_ID, self.tag_value(TAG_PAGE_ID)?).ok()?;
        Some((channel, page))
    }
}

fn record_from_row(row: &sqlx::postgres::PgRow) -> Result<PageEventRecord> {
    let id: Vec<u8> = row.try_get("id")?;
    let author: Vec<u8> = row.try_get("pubkey")?;
    let kind: i32 = row.try_get("kind")?;
    let tags: serde_json::Value = row.try_get("tags")?;
    Ok(PageEventRecord {
        id: <[u8; 32]>::try_from(id.as_slice())
            .map_err(|_| DbError::InvalidData("events.id is not 32 bytes".to_owned()))?,
        kind: u32::try_from(kind)
            .map_err(|_| DbError::InvalidData("events.kind is negative".to_owned()))?,
        author: <[u8; 32]>::try_from(author.as_slice())
            .map_err(|_| DbError::InvalidData("events.pubkey is not 32 bytes".to_owned()))?,
        channel_id: row.try_get("channel_id")?,
        tags: serde_json::from_value(tags)
            .map_err(|error| DbError::InvalidData(format!("events.tags is malformed: {error}")))?,
        content: row.try_get("content")?,
    })
}

/// Load a live (not soft-deleted) event of `community` by id, on the caller's
/// transaction. `None` when it does not exist, is deleted, or belongs to another
/// community.
pub async fn load_event_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    id: &[u8; 32],
) -> Result<Option<PageEventRecord>> {
    let row = sqlx::query(
        "SELECT id, pubkey, kind, channel_id, tags, content FROM events \
         WHERE community_id = $1 AND id = $2 AND deleted_at IS NULL \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(community.as_uuid())
    .bind(id.as_slice())
    .fetch_optional(tx.as_mut())
    .await?;
    row.as_ref().map(record_from_row).transpose()
}

/// Whether a suggestion has been closed (NIP-PG "Accepting a suggestion").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuggestionState {
    /// A live resolution event names the suggestion in its `e` tag.
    pub resolved: bool,
    /// A live revision carries the suggestion in its `suggestion` tag.
    pub applied: bool,
}

impl SuggestionState {
    /// `true` when either a resolution or an applying revision exists.
    pub const fn is_closed(self) -> bool {
        self.resolved || self.applied
    }
}

/// Report whether `suggestion` of page `(channel_id, page_id)` is closed.
///
/// A suggestion is closed by a stored resolution referencing it or by a stored
/// revision carrying its `suggestion` tag; soft-deleted events do not count. The
/// page is matched through the `channel_id` and `d_tag` columns and the tag by
/// exact element comparison (a GIN containment probe narrows the candidates
/// first), so a decoy tag on another event cannot close it.
pub async fn suggestion_state_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    channel_id: Uuid,
    page_id: Uuid,
    suggestion: &[u8; 32],
) -> Result<SuggestionState> {
    let suggestion_hex = hex::encode(suggestion);
    let row = sqlx::query(
        "SELECT \
           EXISTS (SELECT 1 FROM events r \
                   WHERE r.community_id = $1 AND r.kind = $2 AND r.channel_id = $3 \
                     AND r.d_tag = $4 AND r.deleted_at IS NULL AND r.tags @> $5::jsonb \
                     AND EXISTS (SELECT 1 FROM jsonb_array_elements(r.tags) AS t(elem) \
                                 WHERE jsonb_typeof(t.elem) = 'array' \
                                   AND t.elem->>0 = 'e' AND t.elem->>1 = $6)) AS resolved, \
           EXISTS (SELECT 1 FROM events v \
                   WHERE v.community_id = $1 AND v.kind = $7 AND v.channel_id = $3 \
                     AND v.d_tag = $4 AND v.deleted_at IS NULL AND v.tags @> $8::jsonb \
                     AND EXISTS (SELECT 1 FROM jsonb_array_elements(v.tags) AS t(elem) \
                                 WHERE jsonb_typeof(t.elem) = 'array' \
                                   AND t.elem->>0 = 'suggestion' AND t.elem->>1 = $6)) AS applied",
    )
    .bind(community.as_uuid())
    .bind(KIND_PAGE_SUGGESTION_RESOLUTION as i32)
    .bind(channel_id)
    .bind(page_id.to_string())
    .bind(serde_json::json!([["e", &suggestion_hex]]))
    .bind(&suggestion_hex)
    .bind(KIND_PAGE_REVISION as i32)
    .bind(serde_json::json!([["suggestion", &suggestion_hex]]))
    .fetch_one(tx.as_mut())
    .await?;
    Ok(SuggestionState {
        resolved: row.try_get("resolved")?,
        applied: row.try_get("applied")?,
    })
}

/// Store a page event and its mention rows on the caller's transaction.
///
/// Returns the stored event and `false` when the event id already existed (a
/// duplicate submission; nothing was written). The page id is stored in the
/// `d_tag` column (see [`buzz_core::page::is_page_kind`] and
/// `event::extract_d_tag`) so `#h` plus `#d` queries are answered exactly in
/// SQL.
pub async fn insert_page_event_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    event: &nostr::Event,
    channel_id: Uuid,
) -> Result<(StoredEvent, bool)> {
    let kind = u32::from(event.kind.as_u16());
    if !is_page_kind(kind) {
        return Err(DbError::InvalidData(format!(
            "insert_page_event_in_transaction requires a page kind, got {kind}"
        )));
    }
    let (stored, inserted) =
        crate::event::insert_event_in_transaction(tx, community, event, Some(channel_id)).await?;
    if inserted {
        crate::insert_mentions_in_transaction(tx, community, event, Some(channel_id)).await?;
    }
    Ok((stored, inserted))
}

/// After a first revision created a page's row, make the row equal to what a replay
/// of the page computes when the page already has stored history.
///
/// Deleting a page's last live revision removes its row but keeps the deleted
/// events, and the page id can then be created anew. A replay counts every stored
/// revision (deleted ones included) and takes the creator and creation time from
/// the earliest root, so a plain create (count 1, the new revision as creator) would
/// disagree with it the first time anything re-projects the page (NIP-PG "Rebuild
/// Invariant"). The caller holds the page writer lock and has just created the row
/// with [`super::create_page_head_in_transaction`]; a page with no stored history is
/// left alone, and so is a row a replay would not choose this revision as head for
/// (the replay would drop that page, so there is nothing to align to). Returns the
/// aligned row, `None` when nothing changed.
pub async fn align_recreated_page_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    revision: &PageRevisionMeta,
) -> Result<Option<PageRecord>> {
    let has_history: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE community_id = $1 AND kind = $2 \
           AND channel_id = $3 AND d_tag = $4 AND id <> $5)",
    )
    .bind(community.as_uuid())
    .bind(KIND_PAGE_REVISION as i32)
    .bind(revision.channel_id)
    .bind(revision.page_id.to_string())
    .bind(revision.event_id.as_slice())
    .fetch_one(tx.as_mut())
    .await?;
    if !has_history {
        return Ok(None);
    }
    let (revisions, _) = load_revisions(
        tx.as_mut(),
        community,
        revision.channel_id,
        revision.page_id,
        MAX_REPLAY_REVISIONS_PER_PAGE,
    )
    .await?;
    let Some(resolved) = resolve_page(&revisions) else {
        return Ok(None);
    };
    if resolved.head.event_id != revision.event_id {
        return Ok(None);
    }
    let row = sqlx::query(concat!(
        "UPDATE pages SET created_by = $4, created_at = $5, revision_count = $6 \
         WHERE community_id = $1 AND channel_id = $2 AND page_id = $3 AND deleted_at IS NULL \
         RETURNING ",
        page_columns!()
    ))
    .bind(community.as_uuid())
    .bind(revision.channel_id)
    .bind(revision.page_id)
    .bind(resolved.created_by.as_slice())
    .bind(resolved.created_at)
    .bind(resolved.revision_count)
    .fetch_optional(tx.as_mut())
    .await?;
    row.as_ref().map(row_to_record).transpose()
}

/// What [`soft_delete_page_event_in_transaction`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageEventDeletion {
    /// The event is not a page event; nothing was changed. The caller deletes it
    /// through the generic path.
    NotAPageEvent,
    /// The event does not exist or was already deleted.
    NotFound,
    /// The event was soft-deleted. For a revision, `page` is the page's index
    /// row after the head was repaired (`None` when no live revision remains and
    /// the row was removed); for a suggestion or resolution it is `None`.
    Deleted {
        /// The page's index row after deleting a revision.
        page: Option<PageRecord>,
    },
}

/// Soft-delete one page event and repair the head index in the same transaction.
///
/// Deleting a revision may delete the head, so the page is re-projected from its
/// stored events (NIP-PG "Rebuild Invariant"): the head falls back to its `prev`,
/// or the row is removed when no live revision is left. Doing both in one
/// transaction means the index never names a deleted event, and a rebuild by
/// replay agrees with it.
pub async fn soft_delete_page_event_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    community: CommunityId,
    event_id: &[u8],
) -> Result<PageEventDeletion> {
    let row = sqlx::query(
        "SELECT id, pubkey, kind, channel_id, tags, content, deleted_at IS NOT NULL AS is_deleted \
         FROM events WHERE community_id = $1 AND id = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(community.as_uuid())
    .bind(event_id)
    .fetch_optional(tx.as_mut())
    .await?;
    let Some(row) = row else {
        return Ok(PageEventDeletion::NotFound);
    };
    let record = record_from_row(&row)?;
    if !is_page_kind(record.kind) {
        return Ok(PageEventDeletion::NotAPageEvent);
    }
    let is_deleted: bool = row.try_get("is_deleted")?;
    if is_deleted {
        return Ok(PageEventDeletion::NotFound);
    }
    let identity = record.page_identity();
    if let Some((channel_id, page_id)) = identity {
        lock_page_for_write_in_transaction(tx, community, channel_id, page_id).await?;
    }
    let deleted = sqlx::query(
        "UPDATE events SET deleted_at = NOW() \
         WHERE community_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(community.as_uuid())
    .bind(event_id)
    .execute(tx.as_mut())
    .await?
    .rows_affected()
        > 0;
    if !deleted {
        return Ok(PageEventDeletion::NotFound);
    }
    let page = match identity {
        Some((channel_id, page_id)) if record.kind == KIND_PAGE_REVISION => {
            reproject_page_in_transaction(tx, community, channel_id, page_id).await?
        }
        _ => None,
    };
    Ok(PageEventDeletion::Deleted { page })
}

#[cfg(test)]
mod tests {
    use nostr::{EventBuilder, Keys, Kind, Tag};

    use super::*;
    use crate::event::extract_d_tag;

    fn record(tags: &[&[&str]]) -> PageEventRecord {
        PageEventRecord {
            id: [1; 32],
            kind: KIND_PAGE_REVISION,
            author: [2; 32],
            channel_id: None,
            tags: tags
                .iter()
                .map(|tag| tag.iter().map(|part| (*part).to_owned()).collect())
                .collect(),
            content: String::new(),
        }
    }

    #[test]
    fn page_identity_needs_canonical_h_and_d_tags() {
        let (channel, page) = (Uuid::new_v4(), Uuid::new_v4());
        let (c, p) = (channel.to_string(), page.to_string());
        assert_eq!(
            record(&[&["h", &c], &["d", &p]]).page_identity(),
            Some((channel, page))
        );
        assert_eq!(record(&[&["h", &c]]).page_identity(), None);
        assert_eq!(
            record(&[&["h", &c.to_uppercase()], &["d", &p]]).page_identity(),
            None,
            "an uppercase UUID is not a canonical page identity"
        );
        assert_eq!(
            record(&[&["h", &c], &["d", "not-a-uuid"]]).page_identity(),
            None
        );
    }

    #[test]
    fn tag_value_is_the_first_tags_second_element() {
        let rec = record(&[&["title", "first"], &["title", "second"], &["lonely"]]);
        assert_eq!(rec.tag_value("title"), Some("first"));
        assert_eq!(rec.tag_value("lonely"), None);
        assert_eq!(rec.tag_value("absent"), None);
    }

    #[test]
    fn only_page_kinds_and_nip33_store_a_d_tag() {
        let keys = Keys::generate();
        let page = Uuid::new_v4().to_string();
        let with_d = |kind: u16| {
            EventBuilder::new(Kind::Custom(kind), "")
                .tags([Tag::parse(["d", &page]).expect("d tag")])
                .sign_with_keys(&keys)
                .expect("sign")
        };
        for kind in [52000, 52001, 52002, 30023] {
            assert_eq!(extract_d_tag(&with_d(kind)), Some(page.clone()), "{kind}");
        }
        // Neighbouring regular kinds and chat messages must not.
        for kind in [9, 51999, 52003, 40100] {
            assert_eq!(extract_d_tag(&with_d(kind)), None, "{kind}");
        }
    }

    #[test]
    fn a_lock_wait_timeout_is_recognised_only_by_its_sqlstate() {
        assert!(!is_lock_timeout(&DbError::InvalidData("x".to_owned())));
        assert!(!is_lock_timeout(&DbError::Sqlx(sqlx::Error::RowNotFound)));
    }
}
