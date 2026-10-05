//! Command-level tests for `buzz pages`: the production command functions run against
//! an in-process fake relay (`/query` and `/events`), so what the CLI asks for and what
//! it publishes is asserted on the real code path rather than on helpers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

use super::model::tag_value;
use super::*;
use crate::error::exit_code;

const CH: &str = "11111111-1111-4111-8111-111111111111";
const PG: &str = "22222222-2222-4222-8222-222222222222";
const OTHER_PG: &str = "33333333-3333-4333-8333-333333333333";
const UNKNOWN_PG: &str = "44444444-4444-4444-8444-444444444444";

fn id(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

#[derive(Default)]
struct RelayState {
    stored: Mutex<Vec<Value>>,
    posted: Mutex<Vec<Value>>,
    queries: Mutex<Vec<Value>>,
    /// When set, `/events` answers this HTTP status and `{"error": message}`.
    reject: Mutex<Option<(u16, String)>>,
    /// The `message` of an accepted write.
    ack_message: Mutex<String>,
    /// Answer every read with each event twice (REQ-after-write duplicate delivery).
    duplicate_reads: AtomicBool,
}

struct FakeRelay {
    state: Arc<RelayState>,
    client: BuzzClient,
}

fn order(a: &Value, b: &Value) -> std::cmp::Ordering {
    let at = |e: &Value| e["created_at"].as_u64().unwrap_or(0);
    at(b)
        .cmp(&at(a))
        .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
}

fn matches(filter: &Value, event: &Value) -> bool {
    let listed = |key: &str| filter.get(key).and_then(Value::as_array);
    if let Some(kinds) = listed("kinds") {
        if !kinds.contains(&event["kind"]) {
            return false;
        }
    }
    if let Some(ids) = listed("ids") {
        if !ids.contains(&event["id"]) {
            return false;
        }
    }
    for (key, tag) in [("#h", "h"), ("#d", "d"), ("#e", "e")] {
        if let Some(wanted) = listed(key) {
            let value = tag_value(event, tag).map(|v| json!(v));
            if !value.is_some_and(|v| wanted.contains(&v)) {
                return false;
            }
        }
    }
    true
}

async fn query(State(state): State<Arc<RelayState>>, body: Bytes) -> Response {
    let filters: Vec<Value> = serde_json::from_slice(&body).expect("filters");
    let filter = &filters[0];
    state.queries.lock().unwrap().push(filter.clone());
    let mut hits: Vec<Value> = state
        .stored
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches(filter, e))
        .cloned()
        .collect();
    hits.sort_by(order);
    if let (Some(until), Some(before)) = (filter["until"].as_u64(), filter["before_id"].as_str()) {
        hits.retain(|e| {
            let at = e["created_at"].as_u64().unwrap_or(0);
            at < until || (at == until && e["id"].as_str() > Some(before))
        });
    }
    if let Some(limit) = filter["limit"].as_u64() {
        hits.truncate(limit as usize);
    }
    if state.duplicate_reads.load(Ordering::SeqCst) {
        hits = hits.into_iter().flat_map(|e| [e.clone(), e]).collect();
    }
    Response::new(Body::from(Value::Array(hits).to_string()))
}

async fn events(State(state): State<Arc<RelayState>>, body: Bytes) -> Response {
    let event: Value = serde_json::from_slice(&body).expect("event");
    state.posted.lock().unwrap().push(event.clone());
    if let Some((status, message)) = state.reject.lock().unwrap().clone() {
        return Response::builder()
            .status(status)
            .body(Body::from(json!({"error": message}).to_string()))
            .unwrap();
    }
    state.stored.lock().unwrap().push(event.clone());
    let message = state.ack_message.lock().unwrap().clone();
    Response::new(Body::from(
        json!({"event_id": event["id"], "accepted": true, "message": message}).to_string(),
    ))
}

async fn fake_relay() -> FakeRelay {
    let state = Arc::new(RelayState::default());
    let app = Router::new()
        .route("/query", post(query))
        .route("/events", post(events))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let keys =
        nostr::Keys::parse("0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap();
    let client = BuzzClient::new(format!("http://{addr}"), keys, None, None).unwrap();
    FakeRelay { state, client }
}

impl FakeRelay {
    fn seed(&self, event: Value) {
        self.state.stored.lock().unwrap().push(event);
    }

    fn revision(&self, n: u8, prev: Option<u8>, at: u64, title: &str, body: &str) {
        self.revision_in(PG, n, prev, at, title, body);
    }

    fn revision_in(&self, page: &str, n: u8, prev: Option<u8>, at: u64, title: &str, body: &str) {
        let mut tags = vec![
            json!(["h", CH]),
            json!(["d", page]),
            json!(["title", title]),
        ];
        if let Some(p) = prev {
            tags.push(json!(["prev", id(p)]));
        }
        self.seed(
            json!({"id": id(n), "kind": 52000, "created_at": at, "pubkey": id(0xa0),
                         "content": body, "tags": tags, "sig": "00"}),
        );
    }

    fn suggestion(&self, n: u8, base: u8, at: u64, body: &str) {
        self.suggestion_in(PG, n, base, at, body);
    }

    fn suggestion_in(&self, page: &str, n: u8, base: u8, at: u64, body: &str) {
        self.seed(
            json!({"id": id(n), "kind": 52001, "created_at": at, "pubkey": id(0xb0),
                         "content": body, "sig": "00",
                         "tags": [["h", CH], ["d", page], ["base", id(base)]]}),
        );
    }

    fn resolution(&self, n: u8, suggestion: u8, at: u64) {
        self.seed(json!({"id": id(n), "kind": 52002, "created_at": at, "pubkey": id(0xa0),
                         "content": "", "sig": "00",
                         "tags": [["h", CH], ["d", PG], ["e", id(suggestion)], ["status", "rejected"]]}));
    }

    fn posted(&self) -> Vec<Value> {
        self.state.posted.lock().unwrap().clone()
    }

    fn reject_writes(&self, status: u16, message: &str) {
        *self.state.reject.lock().unwrap() = Some((status, message.to_owned()));
    }
}

fn content_arg(text: &str) -> ContentInput<'_> {
    ContentInput {
        file: None,
        content: Some(text),
        allow_empty: false,
    }
}

fn edit_args<'a>(title: Option<&'a str>, base: Option<&'a str>) -> SetArgs<'a> {
    SetArgs {
        channel: CH,
        page: Some(PG),
        new: false,
        title,
        base,
    }
}

fn parse(out: &str) -> Value {
    serde_json::from_str(out).expect("command output is JSON")
}

fn ids_of(out: &str) -> Vec<String> {
    parse(out)
        .as_array()
        .expect("array")
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

// -- get / export ---------------------------------------------------------------------

#[tokio::test]
async fn get_prints_the_head_with_its_metadata_and_marks_older_revisions() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "Old title", "v1");
    relay.revision(2, Some(1), 200, "Runbook", "v2");

    let out = cmd_get(&relay.client, PG, CH, None, &OutputFormat::Json)
        .await
        .unwrap();
    let head = parse(&out);
    assert_eq!(head["revision"], id(2));
    assert_eq!(head["head"], id(2));
    assert_eq!(head["is_head"], true);
    assert_eq!(head["title"], "Runbook");
    assert_eq!(head["content"], "v2");
    assert_eq!(head["author"], id(0xa0));
    assert_eq!(head["updated_at"], 200);

    let out = cmd_get(&relay.client, PG, CH, Some(&id(1)), &OutputFormat::Json)
        .await
        .unwrap();
    let old = parse(&out);
    assert_eq!(old["revision"], id(1));
    assert_eq!(old["head"], id(2), "the current head is still reported");
    assert_eq!(old["is_head"], false);
    assert_eq!(old["content"], "v1");
}

#[tokio::test]
async fn get_reads_the_tip_not_the_first_listed_when_edits_share_a_second() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "first");
    relay.revision(9, Some(1), 100, "T", "second"); // same second, higher id: lists after 1
    let out = cmd_get(&relay.client, PG, CH, None, &OutputFormat::Json)
        .await
        .unwrap();
    assert_eq!(parse(&out)["head"], id(9));
}

#[tokio::test]
async fn get_dedupes_duplicate_delivery() {
    let relay = fake_relay().await;
    relay.state.duplicate_reads.store(true, Ordering::SeqCst);
    relay.revision(1, None, 100, "T", "v1");
    relay.revision(2, Some(1), 200, "T", "v2");
    let out = cmd_get(&relay.client, PG, CH, None, &OutputFormat::Json)
        .await
        .unwrap();
    assert_eq!(parse(&out)["head"], id(2));
}

#[tokio::test]
async fn get_of_an_unknown_page_or_a_foreign_revision_is_not_found() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    relay.revision_in(OTHER_PG, 5, None, 150, "Other", "x");
    let err = cmd_get(&relay.client, UNKNOWN_PG, CH, None, &OutputFormat::Json)
        .await
        .unwrap_err();
    assert!(matches!(err, CliError::NotFound(_)), "{err:?}");
    // Revision 5 exists, but it belongs to another page.
    let err = cmd_get(&relay.client, PG, CH, Some(&id(5)), &OutputFormat::Json)
        .await
        .unwrap_err();
    assert!(matches!(err, CliError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn export_returns_the_head_markdown_byte_for_byte() {
    let relay = fake_relay().await;
    let body = "# Plan\n\nSmörgåsbord  \n- a\n\n```\ncode\n```";
    relay.revision(1, None, 100, "Plan", "old");
    relay.revision(2, Some(1), 200, "Plan", body);
    let exported = cmd_export(&relay.client, PG, CH).await.unwrap();
    assert_eq!(
        exported.content, body,
        "no newline added, nothing normalised"
    );
    assert_eq!(exported.head, id(2));
    assert_eq!(exported.title, "Plan");
}

// -- ls / history ---------------------------------------------------------------------

#[tokio::test]
async fn ls_prints_one_head_per_page_and_says_when_it_left_pages_out() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "Old", "a");
    relay.revision(2, Some(1), 200, "Plan", "b");
    relay.revision_in(OTHER_PG, 5, None, 150, "Other", "c");

    let mut warn = Vec::new();
    let out = cmd_ls(
        &relay.client,
        Some(CH),
        None,
        &OutputFormat::Json,
        &mut warn,
    )
    .await
    .unwrap();
    let pages = parse(&out);
    let heads: Vec<&str> = pages
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["head"].as_str().unwrap())
        .collect();
    assert_eq!(heads, vec![id(2), id(5)], "one head per page, newest first");
    assert_eq!(pages[0]["title"], "Plan");
    assert_eq!(pages[0]["page_id"], PG);
    assert_eq!(pages[0]["channel_id"], CH);
    assert!(warn.is_empty(), "a complete list carries no warning");

    let mut warn = Vec::new();
    let out = cmd_ls(
        &relay.client,
        Some(CH),
        Some(1),
        &OutputFormat::Json,
        &mut warn,
    )
    .await
    .unwrap();
    assert_eq!(parse(&out).as_array().unwrap().len(), 1);
    let warning = String::from_utf8(warn).unwrap();
    assert!(warning.contains("may omit older pages"), "{warning}");

    // The library read is scoped by channel, and always names its kinds.
    let queries = relay.state.queries.lock().unwrap();
    assert_eq!(queries[0]["kinds"], json!([52000]));
    assert_eq!(queries[0]["#h"], json!([CH]));
}

#[tokio::test]
async fn ls_without_a_channel_reads_the_whole_library() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "Plan", "a");
    let mut warn = Vec::new();
    cmd_ls(&relay.client, None, None, &OutputFormat::Compact, &mut warn)
        .await
        .unwrap();
    let queries = relay.state.queries.lock().unwrap();
    assert_eq!(queries[0]["kinds"], json!([52000]));
    assert!(queries[0].get("#h").is_none());
}

#[tokio::test]
async fn history_lists_revisions_and_only_open_suggestions_when_asked() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    relay.revision(2, Some(1), 200, "T", "v2");
    relay.suggestion(10, 2, 250, "open proposal");
    relay.suggestion(11, 2, 260, "declined proposal");
    relay.resolution(20, 11, 270);

    let mut warn = Vec::new();
    let plain = cmd_history(
        &relay.client,
        PG,
        CH,
        false,
        None,
        &OutputFormat::Json,
        &mut warn,
    )
    .await
    .unwrap();
    assert_eq!(ids_of(&plain), vec![id(2), id(1)]);

    let with = cmd_history(
        &relay.client,
        PG,
        CH,
        true,
        None,
        &OutputFormat::Json,
        &mut warn,
    )
    .await
    .unwrap();
    assert_eq!(
        ids_of(&with),
        vec![id(10), id(2), id(1)],
        "newest first; the declined one is gone"
    );
    assert!(warn.is_empty());
}

#[tokio::test]
async fn history_is_bounded_and_says_so() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    relay.revision(2, Some(1), 200, "T", "v2");
    relay.revision(3, Some(2), 300, "T", "v3");
    let mut warn = Vec::new();
    let out = cmd_history(
        &relay.client,
        PG,
        CH,
        false,
        Some(2),
        &OutputFormat::Compact,
        &mut warn,
    )
    .await
    .unwrap();
    assert_eq!(ids_of(&out), vec![id(3), id(2)]);
    assert!(String::from_utf8(warn).unwrap().contains("truncated"));
    assert_eq!(
        parse(&out)[0].get("content"),
        None,
        "compact history carries no bodies"
    );
}

// -- set -------------------------------------------------------------------------------

#[tokio::test]
async fn an_edit_without_base_is_refused_before_any_request() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    let err = cmd_set(
        &relay.client,
        &edit_args(Some("T"), None),
        &content_arg("v2"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    assert_eq!(exit_code(&err), 1);
    assert!(
        relay.posted().is_empty(),
        "nothing is published without a base"
    );
    assert!(
        relay.state.queries.lock().unwrap().is_empty(),
        "nor is anything read"
    );
}

#[tokio::test]
async fn an_edit_publishes_prev_and_keeps_the_base_title_when_none_is_given() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "Runbook", "v1");
    let out = cmd_set(
        &relay.client,
        &edit_args(None, Some(&id(1))),
        &content_arg("v2"),
    )
    .await
    .unwrap();
    assert_eq!(parse(&out)["accepted"], true);
    let posted = relay.posted();
    assert_eq!(posted.len(), 1);
    let rev = &posted[0];
    assert_eq!(rev["kind"], 52000);
    assert_eq!(rev["content"], "v2");
    assert_eq!(tag_value(rev, "title"), Some("Runbook"));
    assert_eq!(tag_value(rev, "prev"), Some(id(1).as_str()));
    assert_eq!(tag_value(rev, "suggestion"), None);
    assert_eq!(
        parse(&out)["event_id"],
        rev["id"],
        "event_id is the new head"
    );
}

#[tokio::test]
async fn an_edit_with_an_explicit_title_renames_the_page() {
    let relay = fake_relay().await;
    cmd_set(
        &relay.client,
        &edit_args(Some("Renamed"), Some(&id(1))),
        &content_arg("v2"),
    )
    .await
    .unwrap();
    assert_eq!(tag_value(&relay.posted()[0], "title"), Some("Renamed"));
}

#[tokio::test]
async fn an_edit_on_a_stale_base_exits_5_and_names_the_head_to_retry_on() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    relay.reject_writes(400, &format!("conflict: stale prev (head {})", id(2)));
    let err = cmd_set(
        &relay.client,
        &edit_args(Some("T"), Some(&id(1))),
        &content_arg("v2"),
    )
    .await
    .unwrap_err();
    assert_eq!(exit_code(&err), 5, "{err:?}");
    assert!(
        err.to_string().contains(&format!("--base {}", id(2))),
        "{err}"
    );
}

#[tokio::test]
async fn relay_invalid_and_restricted_rejections_map_to_exit_1_and_3() {
    let relay = fake_relay().await;
    relay.reject_writes(
        400,
        "invalid: no-op revision (title and content equal the page head)",
    );
    let err = cmd_set(
        &relay.client,
        &edit_args(Some("T"), Some(&id(1))),
        &content_arg("v1"),
    )
    .await
    .unwrap_err();
    assert_eq!(exit_code(&err), 1, "{err:?}");
    relay.reject_writes(403, "restricted: not a channel member");
    let err = cmd_set(
        &relay.client,
        &edit_args(Some("T"), Some(&id(1))),
        &content_arg("v1"),
    )
    .await
    .unwrap_err();
    assert_eq!(exit_code(&err), 3, "{err:?}");
}

#[tokio::test]
async fn a_duplicate_acknowledgement_is_success() {
    let relay = fake_relay().await;
    *relay.state.ack_message.lock().unwrap() = "duplicate: event already stored".into();
    let out = cmd_set(
        &relay.client,
        &edit_args(Some("T"), Some(&id(1))),
        &content_arg("v2"),
    )
    .await
    .expect("a duplicate resubmission has done its job");
    assert!(parse(&out)["message"]
        .as_str()
        .unwrap()
        .starts_with("duplicate:"));
}

#[tokio::test]
async fn a_new_page_is_published_without_prev_and_reports_its_id() {
    let relay = fake_relay().await;
    let args = SetArgs {
        channel: CH,
        page: None,
        new: true,
        title: Some("Runbook"),
        base: None,
    };
    let out = cmd_set(&relay.client, &args, &content_arg("# Runbook"))
        .await
        .unwrap();
    let posted = relay.posted();
    assert_eq!(tag_value(&posted[0], "prev"), None);
    assert_eq!(tag_value(&posted[0], "title"), Some("Runbook"));
    let page_id = parse(&out)["page_id"].as_str().unwrap().to_owned();
    assert_eq!(tag_value(&posted[0], "d"), Some(page_id.as_str()));
    assert_eq!(tag_value(&posted[0], "h"), Some(CH));
}

#[tokio::test]
async fn a_new_page_needs_a_title() {
    let relay = fake_relay().await;
    let args = SetArgs {
        channel: CH,
        page: None,
        new: true,
        title: None,
        base: None,
    };
    let err = cmd_set(&relay.client, &args, &content_arg("x"))
        .await
        .unwrap_err();
    assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    assert!(relay.posted().is_empty());
}

// -- suggest / accept / reject ----------------------------------------------------------

#[tokio::test]
async fn suggest_publishes_a_suggestion_on_the_named_base_and_reports_its_id() {
    let relay = fake_relay().await;
    let out = cmd_suggest(&relay.client, PG, CH, &id(1), &content_arg("proposal"))
        .await
        .unwrap();
    let posted = relay.posted();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["kind"], 52001);
    assert_eq!(tag_value(&posted[0], "base"), Some(id(1).as_str()));
    assert_eq!(posted[0]["content"], "proposal");
    assert_eq!(parse(&out)["suggestion_id"], posted[0]["id"]);
}

#[tokio::test]
async fn accept_publishes_one_revision_on_the_head_and_no_resolution() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "Runbook", "v1");
    relay.suggestion(10, 1, 150, "proposed v2");

    let out = cmd_accept(&relay.client, &id(10), CH, PG).await.unwrap();

    let posted = relay.posted();
    assert_eq!(posted.len(), 1, "accepting is a single event: {posted:?}");
    let rev = &posted[0];
    assert_eq!(rev["kind"], 52000);
    assert_eq!(rev["content"], "proposed v2");
    assert_eq!(
        tag_value(rev, "title"),
        Some("Runbook"),
        "the head's title is kept"
    );
    assert_eq!(tag_value(rev, "prev"), Some(id(1).as_str()));
    assert_eq!(tag_value(rev, "suggestion"), Some(id(10).as_str()));
    assert_eq!(parse(&out)["event_id"], rev["id"]);

    // The head advanced: a fresh read sees the applied revision.
    let head = cmd_get(&relay.client, PG, CH, None, &OutputFormat::Json)
        .await
        .unwrap();
    assert_eq!(parse(&head)["head"], rev["id"]);
}

#[tokio::test]
async fn accept_refuses_a_stale_suggestion_with_exit_5_and_publishes_nothing() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    relay.revision(2, Some(1), 200, "T", "v2"); // the head moved on
    relay.suggestion(10, 1, 150, "proposed from v1");

    let err = cmd_accept(&relay.client, &id(10), CH, PG)
        .await
        .unwrap_err();

    assert_eq!(exit_code(&err), 5, "{err:?}");
    assert!(err.to_string().contains("stale"), "{err}");
    assert!(
        relay.posted().is_empty(),
        "a stale accept must not publish anything"
    );
}

#[tokio::test]
async fn accept_refuses_a_suggestion_that_belongs_to_another_page() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    relay.revision_in(OTHER_PG, 5, None, 100, "Other", "x");
    relay.suggestion_in(OTHER_PG, 10, 5, 150, "proposal for the other page");
    let err = cmd_accept(&relay.client, &id(10), CH, PG)
        .await
        .unwrap_err();
    assert!(matches!(err, CliError::Usage(_)), "{err:?}");
    assert!(relay.posted().is_empty());
}

#[tokio::test]
async fn accept_of_an_unknown_suggestion_is_not_found() {
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    let err = cmd_accept(&relay.client, &id(10), CH, PG)
        .await
        .unwrap_err();
    assert!(matches!(err, CliError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn accept_surfaces_a_relay_conflict_as_exit_5() {
    // The head moved between our read and our write: the relay's CAS answers.
    let relay = fake_relay().await;
    relay.revision(1, None, 100, "T", "v1");
    relay.suggestion(10, 1, 150, "proposal");
    relay.reject_writes(400, "conflict: suggestion is already closed");
    let err = cmd_accept(&relay.client, &id(10), CH, PG)
        .await
        .unwrap_err();
    assert_eq!(exit_code(&err), 5, "{err:?}");
}

#[tokio::test]
async fn reject_publishes_a_rejected_resolution_for_the_suggestion() {
    let relay = fake_relay().await;
    cmd_reject(&relay.client, &id(10), CH, PG).await.unwrap();
    let posted = relay.posted();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["kind"], 52002);
    assert_eq!(tag_value(&posted[0], "e"), Some(id(10).as_str()));
    assert_eq!(tag_value(&posted[0], "status"), Some("rejected"));
    assert_eq!(tag_value(&posted[0], "rev"), None);
}

// -- content intake -----------------------------------------------------------------------

#[tokio::test]
async fn file_content_is_bounded_and_flags_are_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let ok = dir.path().join("ok.md");
    std::fs::write(&ok, "# hi").unwrap();
    let input = ContentInput {
        file: ok.to_str(),
        content: None,
        allow_empty: false,
    };
    assert_eq!(read_content(&input).unwrap(), "# hi");

    let big = dir.path().join("big.md");
    std::fs::write(
        &big,
        "a".repeat(buzz_core::page::MAX_PAGE_CONTENT_BYTES + 1),
    )
    .unwrap();
    let input = ContentInput {
        file: big.to_str(),
        content: None,
        allow_empty: false,
    };
    assert!(matches!(read_content(&input), Err(CliError::Usage(_))));

    let both = ContentInput {
        file: ok.to_str(),
        content: Some("x"),
        allow_empty: false,
    };
    assert!(matches!(read_content(&both), Err(CliError::Usage(_))));

    let missing = ContentInput {
        file: Some("/nonexistent/page.md"),
        content: None,
        allow_empty: false,
    };
    assert!(matches!(read_content(&missing), Err(CliError::Usage(_))));
}

// -- command surface ------------------------------------------------------------------------

#[derive(clap::Parser)]
struct Harness {
    #[command(subcommand)]
    cmd: PagesCmd,
}

fn parses(args: &[&str]) -> bool {
    use clap::Parser;
    Harness::try_parse_from(std::iter::once("pages").chain(args.iter().copied())).is_ok()
}

#[test]
fn the_documented_invocations_parse() {
    let p = id(1);
    assert!(parses(&["ls"]));
    assert!(parses(&["ls", "--channel", CH, "--limit", "5"]));
    assert!(parses(&["get", PG, "--channel", CH]));
    assert!(parses(&["get", PG, "--channel", CH, "--rev", &p]));
    assert!(parses(&[
        "set",
        "--channel",
        CH,
        "--new",
        "--title",
        "T",
        "--file",
        "a.md"
    ]));
    assert!(parses(&[
        "set",
        "--channel",
        CH,
        "--page",
        PG,
        "--base",
        &p,
        "--content",
        "x"
    ]));
    assert!(parses(&[
        "suggest",
        PG,
        "--channel",
        CH,
        "--base",
        &p,
        "--file",
        "a.md"
    ]));
    assert!(parses(&["accept", &p, "--channel", CH, "--page", PG]));
    assert!(parses(&["reject", &p, "--channel", CH, "--page", PG]));
    assert!(parses(&["history", PG, "--channel", CH, "--suggestions"]));
    assert!(parses(&["export", PG, "--channel", CH, "--out", "a.md"]));
}

#[test]
fn required_arguments_are_enforced_at_parse_time() {
    let p = id(1);
    assert!(!parses(&["get", PG]), "--channel is required");
    assert!(
        !parses(&["suggest", PG, "--channel", CH]),
        "--base is required for suggest"
    );
    assert!(
        !parses(&["accept", &p, "--channel", CH]),
        "--page is required for accept"
    );
    assert!(!parses(&["set"]), "--channel is required");
}
