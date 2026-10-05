//! Pure client-side NIP-PG logic for `buzz pages`: head resolution, open-suggestion
//! derivation, relay-rejection mapping, bounded content intake and flag-combination
//! rules. Nothing here performs I/O against the relay, so each rule is unit-tested
//! directly; the command layer in `pages.rs` only wires these to the network.
//!
//! The relay owns the authoritative page index (`buzz-db`'s `resolve_page`, a SQL
//! projection). The CLI cannot link `buzz-db`, so [`resolve_head`] restates the same
//! rule over the revisions it read: the head is the tip (a revision no other revision
//! names as `prev`) with the greatest `created_at`, ties to the lowest event id.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;

use buzz_core::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION};
use buzz_core::page::{MAX_PAGE_CONTENT_BYTES, TAG_PAGE_ID, TAG_PREV, TAG_SUGGESTION};
use serde_json::Value;

use crate::client::normalize_write_response;
use crate::error::CliError;
use crate::validate::{parse_event_id, parse_uuid};

/// Newest revisions read to find a page's head. A page's tips are always among its
/// newest revisions (a tip's successor would be newer), so a small window is exact.
pub(super) const HEAD_WINDOW: u32 = 32;
/// Default and maximum number of revisions `history` prints.
pub(super) const HISTORY_DEFAULT_LIMIT: u32 = 50;
pub(super) const HISTORY_MAX_LIMIT: u32 = 500;
/// Default and maximum number of pages `ls` prints.
pub(super) const LS_DEFAULT_LIMIT: u32 = 50;
pub(super) const LS_MAX_LIMIT: u32 = 200;
/// Revisions `ls` scans to assemble the library. Hitting the bound is reported, not
/// hidden: the list may then omit older pages (NIP-PG rule 9).
pub(super) const LIBRARY_SCAN_MAX: u32 = 1000;
/// Events `history --suggestions` scans to decide which suggestions are still open.
pub(super) const SUGGESTION_SCAN_MAX: u32 = 1000;

// -- Event field access --------------------------------------------------------

/// The value of the first tag named `name`, if it has one.
pub(super) fn tag_value<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
    event
        .get("tags")?
        .as_array()?
        .iter()
        .filter_map(|t| t.as_array())
        .find(|t| t.first().and_then(Value::as_str) == Some(name))
        .and_then(|t| t.get(1))
        .and_then(Value::as_str)
}

pub(super) fn event_id(event: &Value) -> Option<&str> {
    event.get("id").and_then(Value::as_str)
}

pub(super) fn event_kind(event: &Value) -> Option<u64> {
    event.get("kind").and_then(Value::as_u64)
}

pub(super) fn created_at(event: &Value) -> u64 {
    event.get("created_at").and_then(Value::as_u64).unwrap_or(0)
}

pub(super) fn content(event: &Value) -> &str {
    event.get("content").and_then(Value::as_str).unwrap_or("")
}

/// Newest first, ties to the lowest id: the relay's read order.
fn newest_first(a: &Value, b: &Value) -> std::cmp::Ordering {
    created_at(b)
        .cmp(&created_at(a))
        .then_with(|| event_id(a).cmp(&event_id(b)))
}

/// Drop repeated events, keeping the first of each id. A REQ issued right after a
/// write can deliver the same event twice (NIP-PG "Reading").
pub(super) fn dedupe_by_id(events: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    events
        .into_iter()
        .filter(|e| event_id(e).is_some_and(|id| seen.insert(id.to_owned())))
        .collect()
}

// -- Head and library derivation ------------------------------------------------

/// The head of a page, given its revisions (any order, already deduplicated).
///
/// A tip is a revision no other given revision names as `prev`. The head is the tip
/// with the greatest `created_at`, ties to the lowest event id (NIP-PG "Rebuild
/// Invariant"). This is deliberately not "the first event of a newest-first read": two
/// edits made within one second can list the older one first.
pub(super) fn resolve_head<'a, I>(revisions: I) -> Option<&'a Value>
where
    I: IntoIterator<Item = &'a Value>,
    I::IntoIter: Clone,
{
    let revisions = revisions.into_iter();
    let named_as_prev: HashSet<&str> = revisions
        .clone()
        .filter_map(|r| tag_value(r, TAG_PREV))
        .collect();
    let tips = revisions
        .clone()
        .filter(|r| event_id(r).is_some_and(|id| !named_as_prev.contains(id)));
    tips.min_by(|a, b| newest_first(a, b))
        // Unreachable for hash-linked ids (a finite DAG has a tip); kept so a malformed
        // set still resolves to its newest revision instead of to nothing.
        .or_else(|| revisions.min_by(|a, b| newest_first(a, b)))
}

/// The head revision of every page in `revisions`, newest first. Pages are keyed by
/// their `(h, d)` pair; revisions without both tags are skipped.
pub(super) fn library_heads(revisions: &[Value]) -> Vec<&Value> {
    let mut by_page: BTreeMap<(&str, &str), Vec<&Value>> = BTreeMap::new();
    for revision in revisions {
        if let (Some(h), Some(d)) = (tag_value(revision, "h"), tag_value(revision, TAG_PAGE_ID)) {
            by_page.entry((h, d)).or_default().push(revision);
        }
    }
    let mut heads: Vec<&Value> = by_page
        .values()
        .filter_map(|page| resolve_head(page.iter().copied()))
        .collect();
    heads.sort_by(|a, b| newest_first(a, b));
    heads
}

/// Suggestions in `events` that nothing has closed yet.
///
/// A suggestion is closed when a resolution (kind 52002) references it with its `e`
/// tag, or when a revision (kind 52000) carries it in its `suggestion` tag (NIP-PG
/// "Accepting a suggestion"). `events` must hold the page's revisions, suggestions
/// and resolutions: a closer is always newer than its suggestion, so a newest-first
/// window that contains a suggestion contains whatever closed it.
pub(super) fn open_suggestions(events: &[Value]) -> Vec<&Value> {
    let closed: HashSet<&str> = events
        .iter()
        .filter_map(|e| match event_kind(e) {
            Some(k) if k == u64::from(KIND_PAGE_SUGGESTION_RESOLUTION) => tag_value(e, "e"),
            Some(k) if k == u64::from(KIND_PAGE_REVISION) => tag_value(e, TAG_SUGGESTION),
            _ => None,
        })
        .collect();
    events
        .iter()
        .filter(|e| event_kind(e) == Some(u64::from(KIND_PAGE_SUGGESTION)))
        .filter(|e| event_id(e).is_some_and(|id| !closed.contains(id)))
        .collect()
}

/// Why a suggestion cannot be applied on top of `head`, if it cannot.
///
/// A suggestion carries full replacement text, so applying it over a head that has
/// moved on would silently discard the intervening edits: it is stale and refused
/// with a [`CliError::Conflict`] (exit 5) before anything is published.
pub(super) fn check_acceptable(head: &Value, suggestion: &Value) -> Result<(), CliError> {
    let head_id = event_id(head).unwrap_or_default();
    let base = tag_value(suggestion, "base").unwrap_or_default();
    if base != head_id {
        return Err(CliError::Conflict(format!(
            "suggestion is stale: its base {base} is not the page head {head_id}; \
             reject it (`buzz pages reject`) or ask for a fresh suggestion against {head_id}"
        )));
    }
    if content(suggestion) == content(head) {
        return Err(CliError::Usage(
            "suggestion already matches the page head, so there is nothing to apply; \
             close it with `buzz pages reject`"
                .into(),
        ));
    }
    Ok(())
}

// -- Relay answers ---------------------------------------------------------------

/// The head id out of a `... (head <64 hex>)` conflict message, if present.
fn head_from_conflict(detail: &str) -> Option<&str> {
    let start = detail.find("(head ")? + "(head ".len();
    let id = detail.get(start..start + 64)?;
    (id.chars().all(|c| c.is_ascii_hexdigit()) && detail[start + 64..].starts_with(')'))
        .then_some(id)
}

/// Map a relay rejection message to the CLI's error vocabulary (NIP-PG "Errors").
///
/// `conflict:` is the machine-readable "refetch and retry" signal and becomes
/// [`CliError::Conflict`] (exit 5); a stale `prev` names the head the caller should
/// rebuild on. `invalid:` is a rejected input (exit 1) and `restricted:` an
/// authorization failure (exit 3). Anything else is not ours to classify.
pub(super) fn classify_write_rejection(message: &str) -> Option<CliError> {
    let message = message.trim();
    if let Some(detail) = message.strip_prefix("conflict:") {
        let detail = detail.trim();
        let hint = match head_from_conflict(detail) {
            Some(head) if detail.starts_with("stale prev") => format!(
                " — re-read the page (`buzz pages get`) and retry with --base {head}, \
                 or suggest the change instead"
            ),
            _ => String::new(),
        };
        return Some(CliError::Conflict(format!("{detail}{hint}")));
    }
    if message.starts_with("invalid:") {
        return Some(CliError::Usage(message.to_owned()));
    }
    if message.starts_with("restricted:") {
        return Some(CliError::Auth(message.to_owned()));
    }
    None
}

/// Translate a failed `POST /events` (the relay answers a rejection with HTTP 400 and
/// the NIP-PG message as the body) into a classified error; other errors pass through.
pub(super) fn map_write_error(error: CliError) -> CliError {
    if let CliError::Relay { body, .. } = &error {
        if let Some(mapped) = classify_write_rejection(body) {
            return mapped;
        }
    }
    error
}

/// Interpret the relay's `{event_id, accepted, message}` answer to a write.
///
/// `accepted: true` is success, including a `duplicate:` resubmission (the relay
/// already holds the event, so a retried write has done its job). `accepted: false`
/// is classified like any other rejection.
pub(super) fn interpret_write_response(raw: &str) -> Result<String, CliError> {
    let response: Value = serde_json::from_str(raw)
        .map_err(|e| CliError::Other(format!("relay response is not JSON: {e} ({raw})")))?;
    let accepted = response
        .get("accepted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if accepted {
        return Ok(normalize_write_response(raw));
    }
    let message = response
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("");
    Err(classify_write_rejection(message)
        .unwrap_or_else(|| CliError::Other(format!("relay rejected event: {message}"))))
}

// -- Content intake ----------------------------------------------------------------

/// Read at most [`MAX_PAGE_CONTENT_BYTES`] of UTF-8 text from `reader`.
///
/// Reads one byte past the cap so oversize input is refused rather than truncated,
/// and never buffers more than the cap plus one byte.
pub(super) fn read_bounded<R: Read>(reader: R, label: &str) -> Result<String, CliError> {
    let mut buf = Vec::new();
    reader
        .take(MAX_PAGE_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| CliError::Other(format!("failed to read {label}: {e}")))?;
    if buf.len() > MAX_PAGE_CONTENT_BYTES {
        return Err(CliError::Usage(format!(
            "{label} exceeds the page limit of {MAX_PAGE_CONTENT_BYTES} bytes"
        )));
    }
    String::from_utf8(buf).map_err(|_| CliError::Usage(format!("{label} is not valid UTF-8")))
}

/// Refuse content that would publish an empty page, or that exceeds the page cap.
///
/// An empty body is almost always an upstream pipeline step that failed, so it needs
/// an explicit `--allow-empty`.
pub(super) fn check_content(content: String, allow_empty: bool) -> Result<String, CliError> {
    if content.len() > MAX_PAGE_CONTENT_BYTES {
        return Err(CliError::Usage(format!(
            "content exceeds the page limit of {MAX_PAGE_CONTENT_BYTES} bytes (got {})",
            content.len()
        )));
    }
    if content.is_empty() && !allow_empty {
        return Err(CliError::Usage(
            "refusing to publish an empty page body (an upstream pipeline step likely \
             failed); pass --allow-empty to confirm"
                .into(),
        ));
    }
    Ok(content)
}

// -- `set` flag rules -----------------------------------------------------------------

/// What `buzz pages set` is asked to do.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SetTarget {
    /// Create a new page under a fresh id.
    New,
    /// Edit an existing page, based on the revision the caller read.
    Edit {
        page: uuid::Uuid,
        base: nostr::EventId,
    },
}

/// Resolve `--page` / `--new` / `--base` into a [`SetTarget`].
///
/// An edit must name the head it was based on: that is the relay's `prev`
/// compare-and-swap, and silently defaulting to "whatever is newest" would let a stale
/// writer overwrite someone else's edit. Only `--new` omits `--base`.
pub(super) fn resolve_set_target(
    page: Option<&str>,
    new: bool,
    base: Option<&str>,
) -> Result<SetTarget, CliError> {
    match (new, page, base) {
        (true, Some(_), _) => Err(CliError::Usage(
            "--new and --page are mutually exclusive".into(),
        )),
        (true, None, Some(_)) => Err(CliError::Usage(
            "--base only applies when editing an existing page; a new page has no base".into(),
        )),
        (true, None, None) => Ok(SetTarget::New),
        (false, None, _) => Err(CliError::Usage(
            "pass --page <page-id> to edit an existing page, or --new to create one".into(),
        )),
        (false, Some(_), None) => Err(CliError::Usage(
            "--base <head-event-id> is required when editing a page: pass the `head` \
             printed by `buzz pages get <page-id> --channel <uuid>`. The relay rejects the \
             edit if the page has moved since you read it (exit 5)"
                .into(),
        )),
        (false, Some(page), Some(base)) => Ok(SetTarget::Edit {
            page: parse_uuid(page)?,
            base: parse_event_id(base)?,
        }),
    }
}

/// `true` if `event` is a revision of the page `(channel, page)`.
pub(super) fn is_revision_of(event: &Value, channel: &str, page: &str) -> bool {
    event_kind(event) == Some(u64::from(KIND_PAGE_REVISION))
        && tag_value(event, "h") == Some(channel)
        && tag_value(event, TAG_PAGE_ID) == Some(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CH: &str = "11111111-1111-4111-8111-111111111111";
    const PG: &str = "22222222-2222-4222-8222-222222222222";

    fn id(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn rev(n: u8, prev: Option<u8>, at: u64) -> Value {
        let mut tags = vec![json!(["h", CH]), json!(["d", PG]), json!(["title", "T"])];
        if let Some(p) = prev {
            tags.push(json!(["prev", id(p)]));
        }
        json!({"id": id(n), "kind": 52000, "created_at": at, "pubkey": id(0xa0),
               "content": format!("rev {n}"), "tags": tags})
    }

    fn head_id(revs: &[Value]) -> Option<String> {
        resolve_head(revs).and_then(event_id).map(str::to_owned)
    }

    // -- head --

    #[test]
    fn head_of_a_linear_chain_is_its_newest_revision() {
        let revs = [
            rev(3, Some(2), 300),
            rev(2, Some(1), 200),
            rev(1, None, 100),
        ];
        assert_eq!(head_id(&revs), Some(id(3)));
        // The answer does not depend on read order.
        let shuffled = [
            rev(1, None, 100),
            rev(3, Some(2), 300),
            rev(2, Some(1), 200),
        ];
        assert_eq!(head_id(&shuffled), Some(id(3)));
    }

    #[test]
    fn same_second_successor_is_the_head_even_when_its_predecessor_lists_first() {
        // Revision 1 and its successor 9 share a second; the relay reads
        // (created_at DESC, id ASC), so revision 1 lists first. The head is 9.
        let revs = [rev(1, None, 100), rev(9, Some(1), 100)];
        assert_eq!(head_id(&revs), Some(id(9)));
    }

    #[test]
    fn forked_tips_resolve_to_the_greatest_created_at() {
        let revs = [
            rev(1, None, 100),
            rev(2, Some(1), 200),
            rev(3, Some(1), 300),
        ];
        assert_eq!(head_id(&revs), Some(id(3)));
    }

    #[test]
    fn forked_tips_in_the_same_second_resolve_to_the_lowest_id() {
        let revs = [
            rev(1, None, 100),
            rev(7, Some(1), 200),
            rev(5, Some(1), 200),
        ];
        assert_eq!(head_id(&revs), Some(id(5)));
    }

    #[test]
    fn a_deleted_head_is_absent_so_its_prev_becomes_the_tip() {
        // The relay omits deleted events: with revision 3 gone, 2 is the tip again.
        let revs = [rev(2, Some(1), 200), rev(1, None, 100)];
        assert_eq!(head_id(&revs), Some(id(2)));
    }

    #[test]
    fn no_revisions_means_no_head() {
        assert_eq!(resolve_head(&Vec::<Value>::new()), None);
    }

    #[test]
    fn library_has_one_head_per_page_newest_first() {
        let mut other = rev(5, None, 150);
        other["tags"] = json!([
            ["h", CH],
            ["d", "33333333-3333-4333-8333-333333333333"],
            ["title", "Other"]
        ]);
        let revs = vec![rev(1, None, 100), rev(2, Some(1), 200), other];
        let heads: Vec<&str> = library_heads(&revs)
            .into_iter()
            .filter_map(event_id)
            .collect();
        let (two, five) = (id(2), id(5));
        assert_eq!(heads, vec![two.as_str(), five.as_str()]);
    }

    #[test]
    fn same_d_in_two_channels_is_two_pages() {
        let mut elsewhere = rev(2, None, 200);
        elsewhere["tags"] = json!([
            ["h", "44444444-4444-4444-8444-444444444444"],
            ["d", PG],
            ["title", "T"]
        ]);
        let revs = vec![rev(1, None, 100), elsewhere];
        assert_eq!(library_heads(&revs).len(), 2);
    }

    #[test]
    fn dedupe_keeps_the_first_of_each_id() {
        let events = vec![rev(1, None, 100), rev(2, Some(1), 200), rev(1, None, 100)];
        let ids: Vec<String> = dedupe_by_id(events)
            .iter()
            .filter_map(|e| event_id(e).map(str::to_owned))
            .collect();
        assert_eq!(ids, vec![id(1), id(2)]);
    }

    // -- suggestions --

    fn suggestion(n: u8, base: u8, at: u64) -> Value {
        json!({"id": id(n), "kind": 52001, "created_at": at, "pubkey": id(0xb0),
               "content": "proposal", "tags": [["h", CH], ["d", PG], ["base", id(base)]]})
    }

    fn resolution(n: u8, suggestion: u8, status: &str, at: u64) -> Value {
        json!({"id": id(n), "kind": 52002, "created_at": at, "pubkey": id(0xa0),
               "content": "", "tags": [["h", CH], ["d", PG], ["e", id(suggestion)], ["status", status]]})
    }

    fn open_ids(events: &[Value]) -> Vec<String> {
        open_suggestions(events)
            .into_iter()
            .filter_map(|e| event_id(e).map(str::to_owned))
            .collect()
    }

    #[test]
    fn a_suggestion_with_no_closer_is_open() {
        let events = [suggestion(10, 1, 150), rev(1, None, 100)];
        assert_eq!(open_ids(&events), vec![id(10)]);
    }

    #[test]
    fn a_resolution_closes_its_suggestion_whatever_the_status() {
        for status in ["accepted", "rejected"] {
            let events = [resolution(20, 10, status, 300), suggestion(10, 1, 150)];
            assert!(open_ids(&events).is_empty(), "{status} must close it");
        }
    }

    #[test]
    fn a_revision_carrying_the_suggestion_tag_closes_it_without_a_resolution() {
        let mut applying = rev(2, Some(1), 200);
        applying["tags"]
            .as_array_mut()
            .unwrap()
            .push(json!(["suggestion", id(10)]));
        let events = [applying, suggestion(10, 1, 150), rev(1, None, 100)];
        assert!(open_ids(&events).is_empty());
    }

    #[test]
    fn only_the_named_suggestion_is_closed() {
        let events = [
            resolution(20, 10, "rejected", 300),
            suggestion(11, 1, 160),
            suggestion(10, 1, 150),
        ];
        assert_eq!(open_ids(&events), vec![id(11)]);
    }

    #[test]
    fn a_stale_suggestion_is_refused_with_a_conflict_before_anything_is_published() {
        let head = rev(2, Some(1), 200);
        let stale = suggestion(10, 1, 150); // based on revision 1, head is 2
        let err = check_acceptable(&head, &stale).unwrap_err();
        assert!(matches!(err, CliError::Conflict(_)), "{err:?}");
        assert_eq!(crate::error::exit_code(&err), 5);
        assert!(err.to_string().contains(&id(2)), "{err}");
    }

    #[test]
    fn a_suggestion_on_the_head_is_acceptable() {
        let head = rev(2, Some(1), 200);
        assert!(check_acceptable(&head, &suggestion(10, 2, 250)).is_ok());
    }

    #[test]
    fn a_suggestion_equal_to_the_head_is_refused_as_nothing_to_apply() {
        let head = rev(2, Some(1), 200);
        let mut same = suggestion(10, 2, 250);
        same["content"] = head["content"].clone();
        let err = check_acceptable(&head, &same).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    // -- relay rejections --

    #[test]
    fn conflict_rejections_exit_5_and_name_the_head_to_retry_on() {
        let msg = format!("conflict: stale prev (head {})", id(7));
        let err = classify_write_rejection(&msg).expect("classified");
        assert!(matches!(err, CliError::Conflict(_)), "{err:?}");
        assert_eq!(crate::error::exit_code(&err), 5);
        assert!(
            err.to_string().contains(&format!("--base {}", id(7))),
            "{err}"
        );
    }

    #[test]
    fn every_documented_conflict_message_exits_5() {
        for msg in [
            "conflict: page already exists (head 0000000000000000000000000000000000000000000000000000000000000000)",
            "conflict: prev revision not found",
            "conflict: page does not exist",
            "conflict: page is deleted",
            "conflict: suggestion is stale (its base is not the revision's prev)",
            "conflict: suggestion is already closed",
            "conflict: suggestion is already resolved",
            "conflict: suggestion is already applied by another revision",
            "conflict: page is busy with another writer; retry",
        ] {
            let err = classify_write_rejection(msg).expect(msg);
            assert_eq!(crate::error::exit_code(&err), 5, "{msg}");
        }
    }

    #[test]
    fn invalid_rejections_are_input_errors_and_restricted_is_auth() {
        let invalid = classify_write_rejection(
            "invalid: no-op revision (title and content equal the page head)",
        )
        .unwrap();
        assert_eq!(crate::error::exit_code(&invalid), 1);
        let restricted = classify_write_rejection("restricted: not a channel member").unwrap();
        assert_eq!(crate::error::exit_code(&restricted), 3);
        assert!(classify_write_rejection("rate-limited: slow down").is_none());
    }

    #[test]
    fn relay_400_with_a_conflict_body_maps_to_exit_5() {
        let err = map_write_error(CliError::Relay {
            status: 400,
            body: format!("conflict: stale prev (head {})", id(3)),
        });
        assert_eq!(crate::error::exit_code(&err), 5);
        // A relay error that is not a page rejection keeps its own mapping.
        let other = map_write_error(CliError::Relay {
            status: 503,
            body: "unavailable".into(),
        });
        assert_eq!(crate::error::exit_code(&other), 2);
    }

    #[test]
    fn duplicate_resubmission_is_success() {
        let raw =
            json!({"event_id": id(1), "accepted": true, "message": "duplicate: already stored"})
                .to_string();
        let out = interpret_write_response(&raw).expect("duplicate is success");
        assert!(out.contains("\"accepted\":true"), "{out}");
        assert!(
            out.contains("duplicate:"),
            "the message is kept so callers can see it: {out}"
        );
    }

    #[test]
    fn a_non_accepted_answer_is_classified_like_a_rejection() {
        let raw =
            json!({"event_id": id(1), "accepted": false, "message": "conflict: page is deleted"})
                .to_string();
        let err = interpret_write_response(&raw).unwrap_err();
        assert_eq!(crate::error::exit_code(&err), 5);
        let raw = json!({"event_id": id(1), "accepted": false, "message": "mystery"}).to_string();
        assert_eq!(
            crate::error::exit_code(&interpret_write_response(&raw).unwrap_err()),
            4
        );
    }

    // -- content --

    #[test]
    fn content_at_the_cap_is_read_and_one_byte_over_is_refused() {
        let at_cap = "a".repeat(MAX_PAGE_CONTENT_BYTES);
        assert_eq!(
            read_bounded(at_cap.as_bytes(), "stdin").unwrap().len(),
            MAX_PAGE_CONTENT_BYTES
        );
        let over = "a".repeat(MAX_PAGE_CONTENT_BYTES + 1);
        let err = read_bounded(over.as_bytes(), "stdin").unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn a_reader_is_never_drained_past_the_cap() {
        // An endless reader terminates: the intake is bounded, not just validated.
        let err = read_bounded(std::io::repeat(b'a'), "stdin").unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn non_utf8_input_is_a_usage_error() {
        let err = read_bounded(&[0xff_u8, 0xfe][..], "file").unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    }

    #[test]
    fn empty_content_needs_an_explicit_flag() {
        assert!(matches!(
            check_content(String::new(), false),
            Err(CliError::Usage(_))
        ));
        assert_eq!(check_content(String::new(), true).unwrap(), "");
        assert!(check_content("x".repeat(MAX_PAGE_CONTENT_BYTES + 1), true).is_err());
    }

    // -- set flags --

    #[test]
    fn editing_without_a_base_is_refused() {
        let err = resolve_set_target(Some(PG), false, None).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{err:?}");
        assert!(err.to_string().contains("--base"), "{err}");
    }

    #[test]
    fn editing_with_a_base_resolves_to_an_edit() {
        let target = resolve_set_target(Some(PG), false, Some(&id(4))).unwrap();
        assert_eq!(
            target,
            SetTarget::Edit {
                page: PG.parse().unwrap(),
                base: nostr::EventId::parse(&id(4)).unwrap()
            }
        );
    }

    #[test]
    fn a_new_page_takes_no_base_and_no_page_id() {
        assert_eq!(
            resolve_set_target(None, true, None).unwrap(),
            SetTarget::New
        );
        assert!(resolve_set_target(None, true, Some(&id(4))).is_err());
        assert!(resolve_set_target(Some(PG), true, None).is_err());
        assert!(resolve_set_target(None, false, Some(&id(4))).is_err());
    }

    #[test]
    fn malformed_ids_are_usage_errors() {
        assert!(matches!(
            resolve_set_target(Some("nope"), false, Some(&id(4))),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            resolve_set_target(Some(PG), false, Some("zz")),
            Err(CliError::Usage(_))
        ));
    }
}
