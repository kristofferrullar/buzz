//! NIP-PG page ingest (kinds 52000-52002). See `docs/nips/NIP-PG.md`.
//!
//! The generic ingest pipeline has already verified the signature, checked the
//! token scope, resolved the `h` channel, and enforced channel membership and the
//! archived-channel rule when it reaches this module. What remains is page
//! specific and lives here behind two entry points:
//!
//! - [`validate_shape`] is stateless (tags, sizes) and runs before any database
//!   work.
//! - [`store_page_event`] validates against stored pages and writes the event in
//!   one transaction: the page writer lock, the reference reads, the event
//!   insert and, for a revision, the head compare-and-swap all commit together or
//!   not at all (NIP-PG rule 7).
//!
//! Every rejection is a stable string whose prefix is the machine-readable part:
//! `conflict:` (the page moved or the suggestion is closed; refetch and retry),
//! `invalid:` (the event is wrong and retrying it unchanged cannot succeed), and
//! `restricted:` (the community is fenced).

use buzz_core::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION};
use buzz_core::page::{
    is_page_kind, MAX_PAGE_CONTENT_BYTES, MAX_PAGE_TITLE_BYTES, STATUS_ACCEPTED, STATUS_REJECTED,
    TAG_BASE, TAG_PAGE_ID, TAG_PREV, TAG_REV, TAG_STATUS, TAG_SUGGESTION, TAG_TITLE,
};
use buzz_core::tenant::TenantContext;
use buzz_core::StoredEvent;
use buzz_db::page::{self, PageEventRecord, PageHeadError, PageRevisionError, PageRevisionMeta};
use buzz_db::DbError;
use nostr::Event;
use uuid::Uuid;

use super::ingest::{check_channel_membership, IngestError};
use crate::state::AppState;

/// Channel tag. Defined by NIP-29; shared by every page event.
const TAG_CHANNEL: &str = "h";
/// Suggestion reference on a resolution.
const TAG_EVENT: &str = "e";
/// Tags that carry exactly one value (`["name", "value"]`) wherever they appear
/// in a page event.
const SINGLE_VALUE_TAGS: [&str; 9] = [
    TAG_CHANNEL,
    TAG_PAGE_ID,
    TAG_TITLE,
    TAG_PREV,
    TAG_SUGGESTION,
    TAG_BASE,
    TAG_EVENT,
    TAG_STATUS,
    TAG_REV,
];

// -- Stateless shape ---------------------------------------------------------

/// How a resolution closed its suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Applied; `rev` is the revision the resolver published.
    Accepted { rev: [u8; 32] },
    /// Declined.
    Rejected,
}

/// The kind-specific part of a parsed page event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageBody {
    /// `PAGE_REVISION`. `meta` is exactly what the head index and a replay read.
    Revision {
        meta: PageRevisionMeta,
        suggestion: Option<[u8; 32]>,
    },
    /// `PAGE_SUGGESTION` of the revision `base`.
    Suggestion { base: [u8; 32] },
    /// `PAGE_SUGGESTION_RESOLUTION` of `suggestion`.
    Resolution {
        suggestion: [u8; 32],
        outcome: Outcome,
    },
}

/// A page event whose tags and sizes passed the stateless rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedPageEvent {
    /// Channel (`h` tag).
    pub channel_id: Uuid,
    /// Page id (`d` tag).
    pub page_id: Uuid,
    /// Kind-specific fields.
    pub body: PageBody,
}

fn describe(error: &PageRevisionError) -> String {
    match error {
        PageRevisionError::NotARevision(kind) => {
            format!("invalid: kind {kind} is not a page revision")
        }
        PageRevisionError::TagCardinality(tag) if *tag == TAG_PREV => {
            "invalid: page event must carry at most one prev tag".to_owned()
        }
        PageRevisionError::TagCardinality(tag) => {
            format!("invalid: page event must carry exactly one {tag} tag")
        }
        PageRevisionError::MalformedTag { tag, reason }
            if *tag == TAG_TITLE && *reason == "blank" =>
        {
            "invalid: page title must not be blank".to_owned()
        }
        PageRevisionError::MalformedTag { tag, .. } if *tag == TAG_TITLE => {
            format!("invalid: page title exceeds {MAX_PAGE_TITLE_BYTES} bytes")
        }
        PageRevisionError::MalformedTag { tag, reason } => {
            format!("invalid: page {tag} tag is malformed: {reason}")
        }
        PageRevisionError::Timestamp(_) => {
            "invalid: page event created_at is out of range".to_owned()
        }
    }
}

/// Tags named `name`, in event order.
fn tags_named<'a>(event: &'a Event, name: &str) -> Vec<&'a [String]> {
    event
        .tags
        .iter()
        .map(nostr::Tag::as_slice)
        .filter(|tag| tag.first().map(String::as_str) == Some(name))
        .collect()
}

/// The value of the single tag `name`; `None` if absent.
fn optional_value<'a>(event: &'a Event, name: &'static str) -> Result<Option<&'a str>, String> {
    let found = tags_named(event, name);
    match found.as_slice() {
        [] => Ok(None),
        [tag] => Ok(tag.get(1).map(String::as_str)),
        _ => Err(describe(&PageRevisionError::TagCardinality(name))),
    }
}

fn required_value<'a>(event: &'a Event, name: &'static str) -> Result<&'a str, String> {
    optional_value(event, name)?.ok_or_else(|| describe(&PageRevisionError::TagCardinality(name)))
}

fn event_id_value(event: &Event, name: &'static str) -> Result<[u8; 32], String> {
    let value = required_value(event, name)?;
    page::parse_page_event_id(name, value).map_err(|error| describe(&error))
}

fn optional_event_id(event: &Event, name: &'static str) -> Result<Option<[u8; 32]>, String> {
    optional_value(event, name)?
        .map(|value| page::parse_page_event_id(name, value).map_err(|error| describe(&error)))
        .transpose()
}

/// Most tags a page event may carry. Mentions are inserted while the page's
/// writer lock is held, so an unbounded `p` list would let one member stall
/// every other writer to the page.
const MAX_PAGE_TAGS: usize = 100;

/// Apply the stateless NIP-PG rules to a page event.
///
/// Checks the page content cap (byte length), that every tag carrying one value
/// has exactly one, exactly one canonical `h` and `d`, and the per-kind tags. A
/// revision is parsed by the same `PageRevisionMeta::from_event` the head index
/// and a replay use, so the live path never accepts a revision a replay would
/// skip.
pub(crate) fn parse_page_event(event: &Event) -> Result<ParsedPageEvent, String> {
    let kind = u32::from(event.kind.as_u16());
    if !is_page_kind(kind) {
        return Err(format!("invalid: kind {kind} is not a page kind"));
    }
    if event.content.len() > MAX_PAGE_CONTENT_BYTES {
        return Err(format!(
            "invalid: page content exceeds maximum size of {MAX_PAGE_CONTENT_BYTES} bytes (got {})",
            event.content.len()
        ));
    }
    if event.tags.len() > MAX_PAGE_TAGS {
        return Err(format!(
            "invalid: page event exceeds maximum of {MAX_PAGE_TAGS} tags (got {})",
            event.tags.len()
        ));
    }
    for name in SINGLE_VALUE_TAGS {
        if tags_named(event, name).iter().any(|tag| tag.len() != 2) {
            return Err(format!(
                "invalid: page {name} tag must have exactly one value"
            ));
        }
    }
    let channel_id = page::parse_page_uuid(TAG_CHANNEL, required_value(event, TAG_CHANNEL)?)
        .map_err(|error| describe(&error))?;
    let page_id = page::parse_page_uuid(TAG_PAGE_ID, required_value(event, TAG_PAGE_ID)?)
        .map_err(|error| describe(&error))?;
    let e_tags = tags_named(event, TAG_EVENT).len();

    let body = match kind {
        KIND_PAGE_REVISION => {
            if e_tags != 0 {
                return Err("invalid: page revision must not carry an e tag".to_owned());
            }
            let meta = PageRevisionMeta::from_event(event).map_err(|error| describe(&error))?;
            let suggestion = optional_event_id(event, TAG_SUGGESTION)?;
            if suggestion.is_some() && meta.prev.is_none() {
                return Err(
                    "invalid: a revision that applies a suggestion must carry prev".to_owned(),
                );
            }
            PageBody::Revision { meta, suggestion }
        }
        KIND_PAGE_SUGGESTION => {
            if e_tags != 0 {
                return Err("invalid: page suggestion must not carry an e tag".to_owned());
            }
            PageBody::Suggestion {
                base: event_id_value(event, TAG_BASE)?,
            }
        }
        _ => {
            let suggestion = event_id_value(event, TAG_EVENT)?;
            let rev = optional_event_id(event, TAG_REV)?;
            let outcome = match required_value(event, TAG_STATUS)? {
                STATUS_ACCEPTED => Outcome::Accepted {
                    rev: rev.ok_or_else(|| {
                        "invalid: an accepted resolution must carry a rev tag".to_owned()
                    })?,
                },
                STATUS_REJECTED if rev.is_some() => {
                    return Err("invalid: a rejected resolution must not carry a rev tag".to_owned())
                }
                STATUS_REJECTED => Outcome::Rejected,
                _ => {
                    return Err(format!(
                        "invalid: page status must be {STATUS_ACCEPTED} or {STATUS_REJECTED}"
                    ))
                }
            };
            PageBody::Resolution {
                suggestion,
                outcome,
            }
        }
    };
    Ok(ParsedPageEvent {
        channel_id,
        page_id,
        body,
    })
}

/// Reject a page event that fails the stateless rules, before any database work.
pub(crate) fn validate_shape(event: &Event) -> Result<(), IngestError> {
    parse_page_event(event)
        .map(|_| ())
        .map_err(IngestError::Rejected)
}

// -- References --------------------------------------------------------------

/// Check that `reference`, named by `tag`, is a page event of `expected_kind`
/// that belongs to the page `(channel_id, page_id)` (NIP-PG rule 2).
///
/// Both the stored channel scope and the tags are compared, so a reference into
/// another channel or another page of the same channel is rejected the way the
/// reaction and vote handlers reject a target in a different channel.
pub(crate) fn check_reference(
    reference: &PageEventRecord,
    tag: &str,
    expected_kind: u32,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<(), String> {
    if reference.kind != expected_kind {
        let what = if expected_kind == KIND_PAGE_REVISION {
            "revision"
        } else {
            "suggestion"
        };
        return Err(format!("invalid: {tag} must reference a page {what} event"));
    }
    let Some((ref_channel, ref_page)) = reference.page_identity() else {
        return Err(format!("invalid: {tag} event is not a valid page event"));
    };
    if reference.channel_id != Some(channel_id) || ref_channel != channel_id {
        return Err(format!(
            "invalid: {tag} event belongs to a different channel"
        ));
    }
    if ref_page != page_id {
        return Err(format!("invalid: {tag} event belongs to a different page"));
    }
    Ok(())
}

/// Why a page event was refused while it was being validated.
#[derive(Debug)]
enum Refusal {
    /// The final answer.
    Ingest(IngestError),
    /// A reference to an event outside the event's channel. `shown` describes the
    /// event and is only for a caller who can read `channel`; everyone else gets
    /// `hidden`, the answer for an id that was never stored. Without that, the
    /// message would tell any open-channel writer which ids exist in channels they
    /// cannot read (a revision of a private page, a private chat message).
    Foreign {
        channel: Option<Uuid>,
        shown: String,
        hidden: String,
    },
}

impl From<IngestError> for Refusal {
    fn from(error: IngestError) -> Self {
        Self::Ingest(error)
    }
}

/// A final client rejection.
fn reject(message: impl Into<String>) -> Refusal {
    Refusal::Ingest(IngestError::Rejected(message.into()))
}

/// The rejection for a reference that names no event the caller may know about.
fn missing_reference(tag: &str) -> String {
    if tag == TAG_PREV {
        "conflict: prev revision not found".to_owned()
    } else {
        format!("invalid: {tag} event not found")
    }
}

/// [`check_reference`], with a failure on an event outside `channel_id` marked
/// [`Refusal::Foreign`] so the caller's access to that channel decides how much of
/// it is described.
fn check_scoped_reference(
    reference: &PageEventRecord,
    tag: &str,
    expected_kind: u32,
    channel_id: Uuid,
    page_id: Uuid,
) -> Result<(), Refusal> {
    match check_reference(reference, tag, expected_kind, channel_id, page_id) {
        Ok(()) => Ok(()),
        Err(shown) if reference.channel_id != Some(channel_id) => Err(Refusal::Foreign {
            channel: reference.channel_id,
            shown,
            hidden: missing_reference(tag),
        }),
        Err(shown) => Err(Refusal::Ingest(IngestError::Rejected(shown))),
    }
}

/// Whether a scoped token (`None`: an unscoped session) reaches `channel`.
fn token_allows(token_channels: Option<&[Uuid]>, channel: Uuid) -> bool {
    token_channels.is_none_or(|allowed| allowed.contains(&channel))
}

// -- Storing -----------------------------------------------------------------

/// Map a database failure: a lock wait that timed out is retryable
/// (`conflict:`); anything else is a server fault.
fn db_error(error: DbError) -> IngestError {
    if page::is_lock_timeout(&error) {
        IngestError::Rejected("conflict: page is busy with another writer; retry".to_owned())
    } else {
        IngestError::Internal(format!("error: database error: {error}"))
    }
}

/// Map a failed head compare-and-swap onto its stable message.
fn head_error(error: PageHeadError, prev: Option<[u8; 32]>) -> IngestError {
    match error {
        PageHeadError::Conflict { current_head } => {
            IngestError::Rejected(match (prev, current_head) {
                (None, Some(head)) => {
                    format!("conflict: page already exists (head {})", hex::encode(head))
                }
                (_, None) => "conflict: page does not exist".to_owned(),
                (Some(_), Some(head)) => {
                    format!("conflict: stale prev (head {})", hex::encode(head))
                }
            })
        }
        PageHeadError::Deleted => IngestError::Rejected("conflict: page is deleted".to_owned()),
        PageHeadError::Db(error) => db_error(error),
    }
}

async fn load_reference(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    community: buzz_core::CommunityId,
    tag: &str,
    id: &[u8; 32],
) -> Result<PageEventRecord, IngestError> {
    page::load_event_in_transaction(tx, community, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| IngestError::Rejected(missing_reference(tag)))
}

/// Validate a page event against stored pages and store it atomically.
///
/// Returns the stored event and `false` for a duplicate submission (nothing was
/// written). `channel_id` is the channel the generic pipeline resolved from the
/// `h` tag, after membership and archived checks. A revision also advances the
/// page head with a compare-and-swap on `prev`; if that fails the transaction
/// rolls back and the event is not stored.
pub(crate) async fn store_page_event(
    tenant: &TenantContext,
    state: &AppState,
    event: &Event,
    channel_id: Uuid,
    token_channels: Option<&[Uuid]>,
) -> Result<(StoredEvent, bool), IngestError> {
    match store_in_transaction(tenant, state, event, channel_id).await {
        Ok(stored) => Ok(stored),
        Err(Refusal::Ingest(error)) => Err(error),
        Err(Refusal::Foreign {
            channel,
            shown,
            hidden,
        }) => {
            // The transaction ended with the call above, so this lookup holds no
            // second pool connection. The author may be told about an event in a
            // channel they can read (and a scoped token may reach); anything else
            // looks like a missing id.
            let readable = match channel {
                Some(other) if token_allows(token_channels, other) => {
                    check_channel_membership(tenant, state, other, &event.pubkey.to_bytes(), None)
                        .await
                        .is_ok()
                }
                _ => false,
            };
            Err(IngestError::Rejected(if readable { shown } else { hidden }))
        }
    }
}

async fn store_in_transaction(
    tenant: &TenantContext,
    state: &AppState,
    event: &Event,
    channel_id: Uuid,
) -> Result<(StoredEvent, bool), Refusal> {
    let parsed = parse_page_event(event).map_err(IngestError::Rejected)?;
    if parsed.channel_id != channel_id {
        return Err(IngestError::Rejected(
            "invalid: page h tag does not match the event channel".to_owned(),
        )
        .into());
    }
    let community = tenant.community();
    let page_id = parsed.page_id;

    let mut tx = state
        .db
        .begin_event_write_transaction()
        .await
        .map_err(db_error)?;
    // The page lock comes first because it also sets the transaction's lock
    // timeout, so the fence check below cannot wait unboundedly either.
    page::lock_page_for_write_in_transaction(&mut tx, community, channel_id, page_id)
        .await
        .map_err(db_error)?;
    match buzz_deletion::store(&state.db)
        .guard_transaction(&mut tx, community)
        .await
    {
        Ok(()) => {}
        Err(DbError::AccessDenied(reason)) => {
            return Err(reject(format!(
                "restricted: community writes are fenced: {reason}"
            )))
        }
        Err(error) => return Err(db_error(error).into()),
    }

    // An event id we already hold is a retried submission: answer it as a
    // duplicate before any rule that depends on the page's current state, which
    // has moved since the first attempt (a retried accept would otherwise fail
    // as "closed").
    let event_id = *event.id.as_bytes();
    if page::load_event_in_transaction(&mut tx, community, &event_id)
        .await
        .map_err(db_error)?
        .is_some()
    {
        return Ok((StoredEvent::new(event.clone(), Some(channel_id)), false));
    }

    let mut head_move: Option<&PageRevisionMeta> = None;
    let mut no_op = false;
    match &parsed.body {
        PageBody::Revision { meta, suggestion } => {
            no_op =
                validate_revision(&mut tx, community, event, &parsed, meta, *suggestion).await?;
            head_move = Some(meta);
        }
        PageBody::Suggestion { base } => {
            let base_event = load_reference(&mut tx, community, TAG_BASE, base).await?;
            check_scoped_reference(
                &base_event,
                TAG_BASE,
                KIND_PAGE_REVISION,
                channel_id,
                page_id,
            )?;
        }
        PageBody::Resolution {
            suggestion,
            outcome,
        } => {
            validate_resolution(&mut tx, community, event, &parsed, suggestion, *outcome).await?;
        }
    }

    let (stored, inserted) =
        page::insert_page_event_in_transaction(&mut tx, community, event, channel_id)
            .await
            .map_err(db_error)?;
    if !inserted {
        return Ok((stored, false));
    }
    if let Some(meta) = head_move {
        page::record_page_revision_in_transaction(&mut tx, community, meta)
            .await
            .map_err(|error| head_error(error, meta.prev))?;
        // NIP-PG rule 4 compares with the head. `prev` equals the head only once
        // the swap above has held, so a stale `prev` whose content merely equals
        // its own is the retryable `conflict:` it is, not a no-op.
        if no_op {
            return Err(reject(
                "invalid: no-op revision (title and content equal the page head)",
            ));
        }
        if meta.prev.is_none() {
            // A page id created anew after its last live revision was deleted
            // keeps its deleted history: its row must read like a replay of it.
            page::align_recreated_page_in_transaction(&mut tx, community, meta)
                .await
                .map_err(db_error)?;
        }
    }
    tx.commit().await.map_err(|error| db_error(error.into()))?;
    Ok((stored, true))
}

async fn validate_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    community: buzz_core::CommunityId,
    event: &Event,
    parsed: &ParsedPageEvent,
    meta: &PageRevisionMeta,
    suggestion: Option<[u8; 32]>,
) -> Result<bool, Refusal> {
    let Some(prev) = meta.prev else {
        // A first revision is decided by the head index: it conflicts if the
        // page already exists.
        return Ok(false);
    };
    let (channel_id, page_id) = (parsed.channel_id, parsed.page_id);
    let prev_event = load_reference(tx, community, TAG_PREV, &prev).await?;
    check_scoped_reference(
        &prev_event,
        TAG_PREV,
        KIND_PAGE_REVISION,
        channel_id,
        page_id,
    )?;

    if let Some(suggestion_id) = suggestion {
        let suggestion_event =
            load_reference(tx, community, TAG_SUGGESTION, &suggestion_id).await?;
        check_scoped_reference(
            &suggestion_event,
            TAG_SUGGESTION,
            KIND_PAGE_SUGGESTION,
            channel_id,
            page_id,
        )?;
        let state = page::suggestion_state_in_transaction(
            tx,
            community,
            channel_id,
            page_id,
            &suggestion_id,
        )
        .await
        .map_err(db_error)?;
        if state.is_closed() {
            return Err(reject("conflict: suggestion is already closed"));
        }
        let base = suggestion_event
            .tag_value(TAG_BASE)
            .and_then(|value| page::parse_page_event_id(TAG_BASE, value).ok());
        if base != Some(prev) {
            return Err(reject(
                "conflict: suggestion is stale (its base is not the revision's prev)",
            ));
        }
    }

    // NIP-PG rule 4. Equal to `prev`, which the head swap then requires to be the
    // head; the caller rejects it once that swap has held.
    Ok(prev_event.tag_value(TAG_TITLE) == Some(meta.title.as_str())
        && prev_event.content == event.content)
}

async fn validate_resolution(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    community: buzz_core::CommunityId,
    event: &Event,
    parsed: &ParsedPageEvent,
    suggestion: &[u8; 32],
    outcome: Outcome,
) -> Result<(), Refusal> {
    let (channel_id, page_id) = (parsed.channel_id, parsed.page_id);
    let suggestion_event = load_reference(tx, community, TAG_EVENT, suggestion).await?;
    check_scoped_reference(
        &suggestion_event,
        TAG_EVENT,
        KIND_PAGE_SUGGESTION,
        channel_id,
        page_id,
    )?;
    let state =
        page::suggestion_state_in_transaction(tx, community, channel_id, page_id, suggestion)
            .await
            .map_err(db_error)?;
    if state.resolved {
        return Err(reject("conflict: suggestion is already resolved"));
    }
    match outcome {
        Outcome::Rejected if state.applied => {
            Err(reject("conflict: suggestion is already applied"))
        }
        Outcome::Rejected => Ok(()),
        Outcome::Accepted { rev } => {
            let rev_event = load_reference(tx, community, TAG_REV, &rev).await?;
            check_scoped_reference(&rev_event, TAG_REV, KIND_PAGE_REVISION, channel_id, page_id)?;
            if rev_event.author != event.pubkey.to_bytes() {
                return Err(reject(
                    "invalid: rev must be a revision published by the resolver",
                ));
            }
            let applies = rev_event.tag_value(TAG_SUGGESTION);
            let suggestion_hex = hex::encode(suggestion);
            if applies.is_some_and(|value| value != suggestion_hex) {
                return Err(reject("invalid: rev applies a different suggestion"));
            }
            if state.applied && applies.is_none() {
                return Err(reject(
                    "conflict: suggestion is already applied by another revision",
                ));
            }
            Ok(())
        }
    }
}

/// Delete one page event named by a NIP-09 or kind 9005 deletion, repairing the
/// page's head index in the same transaction (NIP-PG "Rebuild Invariant").
///
/// Deleting a revision can delete the head; the head then falls back to its
/// `prev`, or the page disappears when no live revision is left. Doing the
/// delete and the repair in one transaction means the index never names a deleted
/// event, so a rebuild by replay agrees. A failure here leaves the page exactly
/// as it was; like every deletion side effect it is reported to the caller, which
/// logs it.
pub(crate) async fn delete_page_event(
    tenant: &TenantContext,
    state: &AppState,
    event_id: &[u8],
) -> anyhow::Result<()> {
    let mut tx = state.db.begin_event_write_transaction().await?;
    let outcome =
        page::soft_delete_page_event_in_transaction(&mut tx, tenant.community(), event_id).await?;
    match outcome {
        page::PageEventDeletion::Deleted { .. } => {
            tx.commit().await?;
            tracing::info!(target_event = %hex::encode(event_id), "page event deleted");
        }
        page::PageEventDeletion::NotFound | page::PageEventDeletion::NotAPageEvent => {
            tracing::warn!(
                target_event = %hex::encode(event_id),
                "page event already deleted or not found"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
