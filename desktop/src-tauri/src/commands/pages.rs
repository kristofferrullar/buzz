//! Tauri commands that publish NIP-PG page events (kinds 52000-52002).
//!
//! Each command builds exactly one event with the `buzz-sdk` page builders
//! (see [`builders`]), signs it with the user's key and POSTs it through the same
//! `submit_event` path canvas saves and messages use. There is no page-specific
//! signing or transport. The relay stays authoritative: a rejection (`conflict:`
//! for a stale `prev`, size limits, scope) comes back verbatim as the error
//! string, and the webview classifies it.
//!
//! Reads need no command: pages are ordinary events the webview queries over its
//! relay connection.

use tauri::State;

use crate::{app_state::AppState, relay::submit_event};

mod builders;

/// Publish a page revision: create (no `prev`), edit (`prev` is the head the
/// edit is based on) or apply a suggestion (`prev` and `suggestion`).
#[tauri::command]
pub async fn publish_page_revision(
    channel_id: String,
    page_id: String,
    title: String,
    content: String,
    prev: Option<String>,
    suggestion: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let builder = builders::revision_builder(
        &channel_id,
        &page_id,
        &title,
        &content,
        prev.as_deref(),
        suggestion.as_deref(),
    )?;
    let result = submit_event(builder, &state).await?;
    Ok(serde_json::json!({ "event_id": result.event_id }))
}

/// Publish a suggestion: a proposed full-content edit of the revision `base`.
/// It never changes the page head.
#[tauri::command]
pub async fn publish_page_suggestion(
    channel_id: String,
    page_id: String,
    base: String,
    content: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let builder = builders::suggestion_builder(&channel_id, &page_id, &base, &content)?;
    let result = submit_event(builder, &state).await?;
    Ok(serde_json::json!({ "event_id": result.event_id }))
}

/// Close a suggestion with a `rejected` resolution. Accepting is not a command
/// of its own: it is one `publish_page_revision` carrying the `suggestion` tag.
#[tauri::command]
pub async fn reject_page_suggestion(
    channel_id: String,
    page_id: String,
    suggestion: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let builder = builders::rejection_builder(&channel_id, &page_id, &suggestion)?;
    let result = submit_event(builder, &state).await?;
    Ok(serde_json::json!({ "event_id": result.event_id }))
}
