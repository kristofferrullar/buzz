//! `buzz pages` — channel pages with revisions and suggestions (NIP-PG, kinds 52000-52002).
//!
//! A page is a markdown document that lives in a channel and is edited by appending
//! revisions. The CLI is the agent surface of the feature: an agent **suggests** an
//! edit (`suggest`), a person **accepts** (`accept`) or **rejects** (`reject`) it, and a
//! direct rewrite (`set`) is possible but always names the revision it was based on.
//!
//! ## Verbs
//! - `ls [--channel C] [--limit N]` — the library: one head per page, newest first.
//! - `get <page> --channel C [--rev ID]` — a page's head (or one revision) with metadata.
//! - `set --channel C (--page P --base HEAD | --new --title T) (--file F | --content S | stdin)`
//! - `suggest <page> --channel C --base REV (--file F | --content S | stdin)`
//! - `accept <suggestion> --channel C --page P` — apply it: one revision, head advances.
//! - `reject <suggestion> --channel C --page P` — close it with a `rejected` resolution.
//! - `history <page> --channel C [--suggestions] [--limit N]` — revisions, newest first.
//! - `export <page> --channel C [--out FILE]` — the head's markdown, byte for byte.
//!
//! ## Conflicts
//! Every edit carries the head it was based on (`--base`); the relay accepts it only if
//! that is still the head. A rejected edit exits 5 and names the current head so the
//! caller can re-read and retry. `accept` refuses a stale suggestion (its base is not the
//! head) with exit 5 before publishing anything. The pure rules live in [`model`].

mod model;
#[cfg(test)]
mod tests;

use std::io::{IsTerminal, Write};

use buzz_core::kind::{KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION};
use buzz_core::page::TAG_TITLE;
use buzz_sdk::pages::{
    build_page_revision, build_page_suggestion, build_page_suggestion_resolution, PageEdit,
    PageResolution,
};
use clap::Subcommand;
use nostr::{EventBuilder, EventId};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::client::{create_response_with_id_if_accepted, normalize_events, BuzzClient};
use crate::error::CliError;
use crate::validate::{parse_event_id, parse_uuid, sdk_err};
use crate::OutputFormat;

use model::{
    check_acceptable, check_content, content, created_at, dedupe_by_id, event_id, event_kind,
    interpret_write_response, is_revision_of, library_heads, map_write_error, open_suggestions,
    read_bounded, resolve_head, resolve_set_target, tag_value, SetTarget, HEAD_WINDOW,
    HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, LIBRARY_SCAN_MAX, LS_DEFAULT_LIMIT, LS_MAX_LIMIT,
    SUGGESTION_SCAN_MAX,
};

/// `buzz pages` subcommands.
#[derive(Subcommand)]
pub enum PagesCmd {
    /// List pages: one head per page, newest first. Without --channel, every channel you can read.
    #[command(
        after_help = "Examples:\n  buzz pages ls --channel <channel-uuid>\n  buzz --format compact pages ls --limit 20"
    )]
    Ls {
        /// Only pages in this channel (UUID)
        #[arg(long)]
        channel: Option<String>,
        /// Max pages to print (default 50, hard cap 200)
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Read a page: its head revision (or --rev) with title, head id, author and time.
    ///
    /// Pass the printed `head` as --base when you edit or suggest.
    #[command(
        after_help = "Examples:\n  buzz pages get <page-id> --channel <channel-uuid>\n  buzz pages get <page-id> --channel <channel-uuid> --rev <event-id>"
    )]
    Get {
        /// Page id (UUID)
        page: String,
        /// Channel the page lives in (UUID)
        #[arg(long)]
        channel: String,
        /// Read this revision (event id) instead of the head
        #[arg(long)]
        rev: Option<String>,
    },
    /// Create a page (--new) or publish a new revision of one (--page, --base).
    ///
    /// Agents should prefer `suggest`: a revision published here is live at once, a
    /// suggestion waits for a person to accept it. An edit must pass --base, the head
    /// you read; if the page moved since, the relay refuses it and the command exits 5.
    #[command(
        after_help = "Examples:\n  buzz pages set --channel <uuid> --new --title 'Runbook' --file runbook.md\n  buzz pages set --channel <uuid> --page <page-id> --base <head> --file runbook.md\n  cat notes.md | buzz pages set --channel <uuid> --page <page-id> --base <head>"
    )]
    Set {
        /// Channel the page lives in (UUID)
        #[arg(long)]
        channel: String,
        /// Page to edit (UUID). Requires --base.
        #[arg(long)]
        page: Option<String>,
        /// Create a new page with a fresh id instead of editing one
        #[arg(long, default_value_t = false)]
        new: bool,
        /// Page title. Required with --new; an edit keeps its base revision's title when omitted.
        #[arg(long)]
        title: Option<String>,
        /// Head revision (event id) this edit is based on. Required for edits.
        #[arg(long)]
        base: Option<String>,
        /// Read markdown from this file ('-' for stdin)
        #[arg(long)]
        file: Option<String>,
        /// Markdown text ('-' for stdin). Without --file or --content, stdin is read.
        #[arg(long)]
        content: Option<String>,
        /// Allow publishing an empty body (refused by default to catch a failed pipeline)
        #[arg(long, default_value_t = false)]
        allow_empty: bool,
    },
    /// Propose an edit for a person to accept or reject. The page does not change.
    ///
    /// --base is the revision you read; the proposal is full replacement markdown.
    #[command(
        after_help = "Examples:\n  buzz pages suggest <page-id> --channel <uuid> --base <head> --file proposed.md\n  cat proposed.md | buzz pages suggest <page-id> --channel <uuid> --base <head>"
    )]
    Suggest {
        /// Page id (UUID)
        page: String,
        /// Channel the page lives in (UUID)
        #[arg(long)]
        channel: String,
        /// Revision (event id) the proposal edits
        #[arg(long)]
        base: String,
        /// Read markdown from this file ('-' for stdin)
        #[arg(long)]
        file: Option<String>,
        /// Markdown text ('-' for stdin). Without --file or --content, stdin is read.
        #[arg(long)]
        content: Option<String>,
        /// Allow an empty proposal
        #[arg(long, default_value_t = false)]
        allow_empty: bool,
    },
    /// Apply a suggestion: publishes one revision on the current head.
    ///
    /// A stale suggestion (its base is no longer the head) is refused with exit 5.
    Accept {
        /// Suggestion id (event id)
        suggestion: String,
        /// Channel the page lives in (UUID)
        #[arg(long)]
        channel: String,
        /// Page the suggestion belongs to (UUID)
        #[arg(long)]
        page: String,
    },
    /// Decline a suggestion (publishes a `rejected` resolution).
    Reject {
        /// Suggestion id (event id)
        suggestion: String,
        /// Channel the page lives in (UUID)
        #[arg(long)]
        channel: String,
        /// Page the suggestion belongs to (UUID)
        #[arg(long)]
        page: String,
    },
    /// List a page's revisions, newest first; --suggestions adds the still-open suggestions.
    History {
        /// Page id (UUID)
        page: String,
        /// Channel the page lives in (UUID)
        #[arg(long)]
        channel: String,
        /// Also list suggestions that no resolution or revision has closed
        #[arg(long, default_value_t = false)]
        suggestions: bool,
        /// Max revisions to print (default 50, hard cap 500)
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Export the head's markdown, unchanged, to stdout or --out.
    Export {
        /// Page id (UUID)
        page: String,
        /// Channel the page lives in (UUID)
        #[arg(long)]
        channel: String,
        /// Write to this file instead of stdout
        #[arg(long)]
        out: Option<String>,
    },
}

/// Run a `buzz pages` subcommand.
pub async fn dispatch(
    cmd: PagesCmd,
    client: &BuzzClient,
    format: &OutputFormat,
) -> Result<(), CliError> {
    let mut warn = std::io::stderr();
    match cmd {
        PagesCmd::Ls { channel, limit } => {
            let out = cmd_ls(client, channel.as_deref(), limit, format, &mut warn).await?;
            println!("{out}");
        }
        PagesCmd::Get { page, channel, rev } => {
            println!(
                "{}",
                cmd_get(client, &page, &channel, rev.as_deref(), format).await?
            );
        }
        PagesCmd::Set {
            channel,
            page,
            new,
            title,
            base,
            file,
            content,
            allow_empty,
        } => {
            let input = ContentInput {
                file: file.as_deref(),
                content: content.as_deref(),
                allow_empty,
            };
            let target = SetArgs {
                channel: &channel,
                page: page.as_deref(),
                new,
                title: title.as_deref(),
                base: base.as_deref(),
            };
            println!("{}", cmd_set(client, &target, &input).await?);
        }
        PagesCmd::Suggest {
            page,
            channel,
            base,
            file,
            content,
            allow_empty,
        } => {
            let input = ContentInput {
                file: file.as_deref(),
                content: content.as_deref(),
                allow_empty,
            };
            println!(
                "{}",
                cmd_suggest(client, &page, &channel, &base, &input).await?
            );
        }
        PagesCmd::Accept {
            suggestion,
            channel,
            page,
        } => println!(
            "{}",
            cmd_accept(client, &suggestion, &channel, &page).await?
        ),
        PagesCmd::Reject {
            suggestion,
            channel,
            page,
        } => println!(
            "{}",
            cmd_reject(client, &suggestion, &channel, &page).await?
        ),
        PagesCmd::History {
            page,
            channel,
            suggestions,
            limit,
        } => {
            let out = cmd_history(
                client,
                &page,
                &channel,
                suggestions,
                limit,
                format,
                &mut warn,
            )
            .await?;
            println!("{out}");
        }
        PagesCmd::Export { page, channel, out } => {
            let exported = cmd_export(client, &page, &channel).await?;
            match out {
                Some(path) => {
                    std::fs::write(&path, &exported.content)
                        .map_err(|e| CliError::Other(format!("failed to write {path:?}: {e}")))?;
                    println!(
                        "{}",
                        json!({
                            "out": path,
                            "head": exported.head,
                            "title": exported.title,
                            "bytes": exported.content.len(),
                        })
                    );
                }
                None => {
                    let mut stdout = std::io::stdout().lock();
                    stdout
                        .write_all(exported.content.as_bytes())
                        .and_then(|()| stdout.flush())
                        .map_err(|e| CliError::Other(format!("failed to write stdout: {e}")))?;
                }
            }
        }
    }
    Ok(())
}

// -- Reading -------------------------------------------------------------------------

fn parse_response(raw: &str) -> Result<Vec<Value>, CliError> {
    serde_json::from_str(raw)
        .map_err(|e| CliError::Other(format!("failed to parse query response: {e}")))
}

/// Fetch one event by id, `None` if the relay does not hold an event of that kind.
async fn fetch_event(
    client: &BuzzClient,
    id: &EventId,
    kind: u32,
) -> Result<Option<Value>, CliError> {
    let hex = id.to_hex();
    let filter = json!({"ids": [hex], "kinds": [kind], "limit": 1});
    let events = parse_response(&client.query(&filter).await?)?;
    Ok(events
        .into_iter()
        .find(|e| event_id(e) == Some(hex.as_str()) && event_kind(e) == Some(u64::from(kind))))
}

/// The newest `limit` revisions of a page, deduplicated, newest first.
async fn fetch_revisions(
    client: &BuzzClient,
    channel: &str,
    page: &str,
    limit: u32,
) -> Result<Vec<Value>, CliError> {
    let filter = json!({"kinds": [KIND_PAGE_REVISION], "#h": [channel], "#d": [page]});
    let events = client.query_paginated(filter, limit).await?;
    Ok(dedupe_by_id(events)
        .into_iter()
        .filter(|e| is_revision_of(e, channel, page))
        .collect())
}

/// A page's revisions window and the head resolved from it.
struct PageView {
    revisions: Vec<Value>,
}

impl PageView {
    async fn load(client: &BuzzClient, channel: &str, page: &str) -> Result<Self, CliError> {
        let revisions = fetch_revisions(client, channel, page, HEAD_WINDOW).await?;
        if revisions.is_empty() {
            return Err(CliError::NotFound(format!(
                "page {page} not found in channel {channel}"
            )));
        }
        Ok(Self { revisions })
    }

    fn head(&self) -> Result<&Value, CliError> {
        resolve_head(&self.revisions)
            .ok_or_else(|| CliError::Other("page has revisions but no resolvable head".into()))
    }
}

/// Canonical (lowercase, hyphenated) channel and page ids: the form the relay indexes.
fn canonical_ids(channel: &str, page: &str) -> Result<(Uuid, Uuid), CliError> {
    Ok((parse_uuid(channel)?, parse_uuid(page)?))
}

fn title_of(event: &Value) -> &str {
    tag_value(event, TAG_TITLE).unwrap_or_default()
}

/// `buzz pages get`.
async fn cmd_get(
    client: &BuzzClient,
    page: &str,
    channel: &str,
    rev: Option<&str>,
    format: &OutputFormat,
) -> Result<String, CliError> {
    let (channel_id, page_id) = canonical_ids(channel, page)?;
    let (channel, page) = (channel_id.to_string(), page_id.to_string());
    let view = PageView::load(client, &channel, &page).await?;
    let head = view.head()?;
    let shown = match rev {
        None => head.clone(),
        Some(rev) => {
            let rev_id = parse_event_id(rev)?;
            let in_window = view
                .revisions
                .iter()
                .find(|e| event_id(e) == Some(rev_id.to_hex().as_str()));
            let found = match in_window {
                Some(event) => Some(event.clone()),
                None => fetch_event(client, &rev_id, KIND_PAGE_REVISION).await?,
            };
            found
                .filter(|e| is_revision_of(e, &channel, &page))
                .ok_or_else(|| {
                    CliError::NotFound(format!("revision {rev} is not a revision of page {page}"))
                })?
        }
    };
    let shown_id = event_id(&shown).unwrap_or_default();
    let head_id = event_id(head).unwrap_or_default();
    let view_json = match format {
        OutputFormat::Compact => json!({
            "page_id": page,
            "revision": shown_id,
            "head": head_id,
            "title": title_of(&shown),
            "content": content(&shown),
        }),
        OutputFormat::Json => json!({
            "page_id": page,
            "channel_id": channel,
            "revision": shown_id,
            "head": head_id,
            "is_head": shown_id == head_id,
            "prev": tag_value(&shown, "prev"),
            "title": title_of(&shown),
            "author": shown.get("pubkey").and_then(Value::as_str).unwrap_or_default(),
            "updated_at": created_at(&shown),
            "content": content(&shown),
        }),
    };
    Ok(view_json.to_string())
}

/// `buzz pages ls`.
async fn cmd_ls(
    client: &BuzzClient,
    channel: Option<&str>,
    limit: Option<u32>,
    format: &OutputFormat,
    warn: &mut impl Write,
) -> Result<String, CliError> {
    let limit = bounded_limit(limit, LS_DEFAULT_LIMIT, LS_MAX_LIMIT)?;
    let mut filter = json!({"kinds": [KIND_PAGE_REVISION]});
    if let Some(channel) = channel {
        filter["#h"] = json!([parse_uuid(channel)?.to_string()]);
    }
    // One probe revision past the scan bound tells "exactly the bound" from "more".
    let mut revisions = dedupe_by_id(client.query_paginated(filter, LIBRARY_SCAN_MAX + 1).await?);
    let scan_truncated = revisions.len() > LIBRARY_SCAN_MAX as usize;
    revisions.truncate(LIBRARY_SCAN_MAX as usize);

    let heads = library_heads(&revisions);
    let listed = heads.len().min(limit as usize);
    if scan_truncated || heads.len() > listed {
        let _ = writeln!(
            warn,
            "{}",
            json!({"warning": format!(
                "the list may omit older pages: scanned the newest {} revisions and printed {} of {} pages (raise --limit up to {LS_MAX_LIMIT}, or narrow with --channel)",
                revisions.len(), listed, heads.len()
            )})
        );
    }
    let entries: Vec<Value> = heads
        .into_iter()
        .take(listed)
        .map(|head| {
            let mut entry = json!({
                "page_id": tag_value(head, "d").unwrap_or_default(),
                "channel_id": tag_value(head, "h").unwrap_or_default(),
                "title": title_of(head),
                "head": event_id(head).unwrap_or_default(),
            });
            if matches!(format, OutputFormat::Json) {
                entry["updated_by"] = head.get("pubkey").cloned().unwrap_or_default();
                entry["updated_at"] = json!(created_at(head));
            }
            entry
        })
        .collect();
    Ok(Value::Array(entries).to_string())
}

/// `buzz pages history`.
async fn cmd_history(
    client: &BuzzClient,
    page: &str,
    channel: &str,
    with_suggestions: bool,
    limit: Option<u32>,
    format: &OutputFormat,
    warn: &mut impl Write,
) -> Result<String, CliError> {
    let limit = bounded_limit(limit, HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT)?;
    let (channel_id, page_id) = canonical_ids(channel, page)?;
    let (channel, page) = (channel_id.to_string(), page_id.to_string());

    let mut listed: Vec<Value> = if with_suggestions {
        // Revisions, suggestions and resolutions in one newest-first window: a closer
        // is newer than its suggestion, so the window holds whatever closed each one.
        let filter = json!({
            "kinds": [KIND_PAGE_REVISION, KIND_PAGE_SUGGESTION, buzz_core::kind::KIND_PAGE_SUGGESTION_RESOLUTION],
            "#h": [channel], "#d": [page],
        });
        let mut events = dedupe_by_id(
            client
                .query_paginated(filter, SUGGESTION_SCAN_MAX + 1)
                .await?,
        );
        if events.len() > SUGGESTION_SCAN_MAX as usize {
            events.truncate(SUGGESTION_SCAN_MAX as usize);
            let _ = writeln!(
                warn,
                "{}",
                json!({"warning": format!("scanned only the newest {SUGGESTION_SCAN_MAX} page events; older open suggestions may be missing")})
            );
        }
        let open: Vec<Value> = open_suggestions(&events).into_iter().cloned().collect();
        let mut revisions: Vec<Value> = events
            .into_iter()
            .filter(|e| is_revision_of(e, &channel, &page))
            .collect();
        warn_if_revisions_cut(&mut revisions, limit, warn);
        revisions.extend(open);
        revisions
    } else {
        let mut revisions = fetch_revisions(client, &channel, &page, limit + 1).await?;
        warn_if_revisions_cut(&mut revisions, limit, warn);
        revisions
    };
    if listed.is_empty() {
        return Err(CliError::NotFound(format!(
            "page {page} not found in channel {channel}"
        )));
    }
    listed.sort_by(|a, b| {
        created_at(b)
            .cmp(&created_at(a))
            .then_with(|| event_id(a).cmp(&event_id(b)))
    });
    Ok(match format {
        OutputFormat::Json => normalize_events(&listed),
        OutputFormat::Compact => {
            Value::Array(listed.iter().map(compact_history_event).collect()).to_string()
        }
    })
}

/// Keep the newest `limit` revisions and say so when older ones were left out.
fn warn_if_revisions_cut(revisions: &mut Vec<Value>, limit: u32, warn: &mut impl Write) {
    if revisions.len() > limit as usize {
        revisions.truncate(limit as usize);
        let _ = writeln!(
            warn,
            "{}",
            json!({"warning": format!("history truncated to the newest {limit} revisions; raise --limit (max {HISTORY_MAX_LIMIT}) to see more")})
        );
    }
}

/// History without bodies, for scanning: identifies each event and what it points at.
fn compact_history_event(event: &Value) -> Value {
    let mut out = json!({
        "id": event_id(event).unwrap_or_default(),
        "kind": event_kind(event).unwrap_or_default(),
        "pubkey": event.get("pubkey").and_then(Value::as_str).unwrap_or_default(),
        "created_at": created_at(event),
    });
    for tag in [TAG_TITLE, "prev", "base", "suggestion"] {
        if let Some(value) = tag_value(event, tag) {
            out[tag] = json!(value);
        }
    }
    out
}

fn bounded_limit(limit: Option<u32>, default: u32, max: u32) -> Result<u32, CliError> {
    match limit {
        None => Ok(default),
        Some(0) => Err(CliError::Usage("--limit must be at least 1".into())),
        Some(n) => Ok(n.min(max)),
    }
}

/// A page's head, ready to write out.
struct Exported {
    head: String,
    title: String,
    content: String,
}

/// `buzz pages export`.
async fn cmd_export(client: &BuzzClient, page: &str, channel: &str) -> Result<Exported, CliError> {
    let (channel_id, page_id) = canonical_ids(channel, page)?;
    let view = PageView::load(client, &channel_id.to_string(), &page_id.to_string()).await?;
    let head = view.head()?;
    Ok(Exported {
        head: event_id(head).unwrap_or_default().to_owned(),
        title: title_of(head).to_owned(),
        content: content(head).to_owned(),
    })
}

// -- Writing -------------------------------------------------------------------------

/// Where the markdown for `set` / `suggest` comes from.
struct ContentInput<'a> {
    file: Option<&'a str>,
    content: Option<&'a str>,
    allow_empty: bool,
}

/// The flags that pick the page `set` writes.
struct SetArgs<'a> {
    channel: &'a str,
    page: Option<&'a str>,
    new: bool,
    title: Option<&'a str>,
    base: Option<&'a str>,
}

fn read_stdin() -> Result<String, CliError> {
    read_bounded(std::io::stdin().lock(), "stdin")
}

/// Resolve and bound the markdown body. Reads stdin when neither flag names a source,
/// but never waits on an interactive terminal.
fn read_content(input: &ContentInput<'_>) -> Result<String, CliError> {
    let body = match (input.file, input.content) {
        (Some(_), Some(_)) => {
            return Err(CliError::Usage(
                "--file and --content are mutually exclusive".into(),
            ))
        }
        (Some("-"), None) | (None, Some("-")) => read_stdin()?,
        (Some(path), None) => {
            let file = std::fs::File::open(path)
                .map_err(|e| CliError::Usage(format!("failed to open {path:?}: {e}")))?;
            read_bounded(file, path)?
        }
        (None, Some(text)) => text.to_owned(),
        (None, None) => {
            if std::io::stdin().is_terminal() {
                return Err(CliError::Usage(
                    "no content: pass --file, --content, or pipe markdown on stdin".into(),
                ));
            }
            read_stdin()?
        }
    };
    check_content(body, input.allow_empty)
}

/// Sign, submit and interpret a page event. Returns the relay's normalized answer and
/// the event id (the new head for a revision, the suggestion id for a suggestion).
async fn publish(client: &BuzzClient, builder: EventBuilder) -> Result<(String, String), CliError> {
    let event = client.sign_event(builder)?;
    let id = event.id.to_hex();
    let raw = client.submit_event(event).await.map_err(map_write_error)?;
    Ok((interpret_write_response(&raw)?, id))
}

/// The title of the revision an edit is based on, for an edit that gives no --title.
async fn base_title(
    client: &BuzzClient,
    channel: &Uuid,
    page: &Uuid,
    base: &EventId,
) -> Result<String, CliError> {
    let found = fetch_event(client, base, KIND_PAGE_REVISION)
        .await?
        .filter(|e| is_revision_of(e, &channel.to_string(), &page.to_string()));
    match found {
        Some(revision) => Ok(title_of(&revision).to_owned()),
        None => Err(CliError::NotFound(format!(
            "--base {} is not a revision of page {page}; read the head with `buzz pages get`",
            base.to_hex()
        ))),
    }
}

/// `buzz pages set`.
async fn cmd_set(
    client: &BuzzClient,
    args: &SetArgs<'_>,
    input: &ContentInput<'_>,
) -> Result<String, CliError> {
    let channel = parse_uuid(args.channel)?;
    let target = resolve_set_target(args.page, args.new, args.base)?;
    if target == SetTarget::New && args.title.is_none() {
        return Err(CliError::Usage("--title is required with --new".into()));
    }
    let body = read_content(input)?;
    match target {
        SetTarget::New => {
            let page = Uuid::new_v4();
            let title = args.title.unwrap_or_default();
            let builder = build_page_revision(channel, page, title, &body, PageEdit::Create)
                .map_err(sdk_err)?;
            let (resp, _) = publish(client, builder).await?;
            Ok(create_response_with_id_if_accepted(
                &resp,
                "page_id",
                &page.to_string(),
            ))
        }
        SetTarget::Edit { page, base } => {
            let title = match args.title {
                Some(title) => title.to_owned(),
                None => base_title(client, &channel, &page, &base).await?,
            };
            let builder =
                build_page_revision(channel, page, &title, &body, PageEdit::Edit { prev: base })
                    .map_err(sdk_err)?;
            Ok(publish(client, builder).await?.0)
        }
    }
}

/// `buzz pages suggest`.
async fn cmd_suggest(
    client: &BuzzClient,
    page: &str,
    channel: &str,
    base: &str,
    input: &ContentInput<'_>,
) -> Result<String, CliError> {
    let (channel, page) = canonical_ids(channel, page)?;
    let base = parse_event_id(base)?;
    let body = read_content(input)?;
    let builder = build_page_suggestion(channel, page, base, &body).map_err(sdk_err)?;
    let (resp, id) = publish(client, builder).await?;
    Ok(create_response_with_id_if_accepted(
        &resp,
        "suggestion_id",
        &id,
    ))
}

/// `buzz pages accept`: apply a suggestion as one revision on the current head.
///
/// The suggestion must still be based on the head. A stale one is refused before
/// anything is published, and no resolution is ever published here: the revision's
/// `suggestion` tag closes the suggestion on its own (NIP-PG "Accepting a suggestion").
async fn cmd_accept(
    client: &BuzzClient,
    suggestion: &str,
    channel: &str,
    page: &str,
) -> Result<String, CliError> {
    let (channel_id, page_id) = canonical_ids(channel, page)?;
    let (channel, page) = (channel_id.to_string(), page_id.to_string());
    let suggestion_id = parse_event_id(suggestion)?;
    let proposal = fetch_event(client, &suggestion_id, KIND_PAGE_SUGGESTION)
        .await?
        .ok_or_else(|| CliError::NotFound(format!("suggestion {suggestion} not found")))?;
    if tag_value(&proposal, "h") != Some(channel.as_str())
        || tag_value(&proposal, "d") != Some(page.as_str())
    {
        return Err(CliError::Usage(format!(
            "suggestion {suggestion} does not belong to page {page} in channel {channel}"
        )));
    }
    let view = PageView::load(client, &channel, &page).await?;
    let head = view.head()?;
    check_acceptable(head, &proposal)?;
    let prev = parse_event_id(event_id(head).unwrap_or_default())?;
    let builder = build_page_revision(
        channel_id,
        page_id,
        title_of(head),
        content(&proposal),
        PageEdit::ApplySuggestion {
            prev,
            suggestion: suggestion_id,
        },
    )
    .map_err(sdk_err)?;
    Ok(publish(client, builder).await?.0)
}

/// `buzz pages reject`.
async fn cmd_reject(
    client: &BuzzClient,
    suggestion: &str,
    channel: &str,
    page: &str,
) -> Result<String, CliError> {
    let (channel, page) = canonical_ids(channel, page)?;
    let suggestion = parse_event_id(suggestion)?;
    let builder =
        build_page_suggestion_resolution(channel, page, suggestion, PageResolution::Rejected)
            .map_err(sdk_err)?;
    Ok(publish(client, builder).await?.0)
}
