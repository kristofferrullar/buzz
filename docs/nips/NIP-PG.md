NIP-PG
======

Pages
-----

`draft` `fork-local`

This NIP defines channel-scoped, revisioned documents ("pages") and a suggestion
flow so agents can propose edits that members accept or reject. Kind numbers are
provisional and sit in a fork-private block (see
[pages-fork-upgrade.md](../pages-fork-upgrade.md)).

## Kinds

| Kind  | Name | Storage |
|-------|------|---------|
| 52000 | `PAGE_REVISION` | regular, stored, `h`-scoped |
| 52001 | `PAGE_SUGGESTION` | regular, stored, `h`-scoped |
| 52002 | `PAGE_SUGGESTION_RESOLUTION` | regular, stored, `h`-scoped |

Regular (not replaceable) kinds are used on purpose: NIP-33 replaceable events
are keyed per author, which cannot represent one page edited by many authors,
and an append-only log gives history and conflict detection for free.

## Identity

A page is identified by the pair `(h, d)`: the channel it lives in and its page
id (uuid v4). The same `d` in two channels is two different pages. Every event
that references another page event (`prev`, `base`, `e`, `suggestion`, `rev`)
MUST reference an event of the same `(h, d)`.

## Tags

Common: `["h", "<channel uuid>"]`, `["d", "<page id, uuid v4>"]`. Both ids are
written as canonical lowercase hyphenated UUIDs and event ids as 64 lowercase
hex characters; the relay indexes only events written that way, and rejects
others. Every tag defined here (`h`, `d`, `title`, `prev`, `suggestion`, `base`,
`e`, `status`, `rev`) has exactly one value: `["name", "value"]`. Other tags are
ignored, except that only a resolution may carry an `e` tag (a revision or
suggestion with one is rejected, so a page event can never be read as a NIP-10
reply).

`PAGE_REVISION`
- `["prev", "<event id of the revision this was based on>"]` — omitted on the
  first revision of a page; required otherwise. At most one.
- `["title", "<utf-8 title>"]` — required, exactly one, non-blank, at most 256
  bytes.
- `["suggestion", "<event id>"]` — optional; present when the revision applies
  a suggestion (see Accepting a suggestion).
- `content`: markdown, UTF-8.

`PAGE_SUGGESTION`
- `["base", "<event id of the revision the suggestion edits>"]` — required.
- `content`: proposed full markdown. Clients derive the diff against `base`.

`PAGE_SUGGESTION_RESOLUTION`
- `["e", "<suggestion event id>"]`, `["status", "accepted" | "rejected"]`.
- `["rev", "<resulting revision id>"]` — required when accepted, and rejected
  on a `rejected` resolution.

## Relay Rules

1. **Who may write.** The author MUST be able to write to the channel: the same
   gate as channel messages (a member, or any authenticated user for an `open`
   channel), and archived channels reject (`invalid: channel is archived`). A
   scoped API token needs the `channels:write` scope; see "Write scope" below.
2. **Scope.** Every page event MUST carry exactly one `h` tag and one `d` tag,
   and every referenced event (`prev`, `base`, `e`, `suggestion`, `rev`) MUST
   exist (not deleted), be of the right kind, and share the event's `(h, d)`:
   another channel or another page of the same channel is rejected, like a
   reaction or vote aimed at an event in a different channel. References are
   resolved inside the event's community only. A reference to an event in a
   channel the author cannot read (or that has no channel) is answered exactly
   as an id that was never stored (`... not found`), so a rejection never
   reveals which events exist elsewhere; "different channel" and "wrong kind"
   are reported only for events in the event's own channel or a channel the
   author can read.
3. **Head.** A page's head is the newest revision in its `prev` chain. A new
   revision is accepted only if `prev` equals the current head (or the page does
   not exist and `prev` is absent). Otherwise the relay rejects with a
   `conflict:` message and stores nothing. A `prev` that was never stored is the
   same conflict.
4. Revisions whose content and title equal the head's are rejected as no-ops.
   Rule 3 is decided first: a revision on a stale `prev` is a `conflict:` even
   when its text equals that `prev`'s, because it is not equal to the head.
5. Content is bounded to 64 KiB (65,536 **bytes**, not characters) for page
   kinds; oversize is rejected, not truncated. The title is non-blank and at
   most 256 bytes. (The relay's generic event cap is larger; page kinds are held
   to this tighter limit by a page-specific check.)
6. A suggestion never changes the head. Any writer may publish one.
7. Storing the event and advancing the head are one atomic operation: one
   database transaction holds the event insert and the head compare-and-swap, so
   a failure of either leaves neither. The writers of one page are serialized;
   a writer that waits more than 5 seconds for the page gets
   `conflict: page is busy with another writer; retry` and nothing is stored.
   A resubmitted event the relay already holds is answered `duplicate:` (accepted,
   not stored twice) before any state-dependent rule runs, so a retried accept
   does not fail as "already closed".
8. **Queryability.** `#h` plus `#d` page queries MUST be answered exactly, not
   by filtering after a row limit, so a quiet page's history is complete in a
   busy channel. The relay stores the page id in the event's `d_tag` column
   (as it does for NIP-33 kinds) and pushes `#d` into SQL for REQ, COUNT, and the
   HTTP `/query` and `/count` bridges. The guarantee covers filters whose `kinds`
   are all page kinds (kinds 52000-52002, optionally with NIP-33 kinds); a
   kindless `#d` filter is answered like any other generic tag, after the limit.
9. **Library reads.** A filter for `PAGE_REVISION` with no `#h` is scoped to the
   channels the reader can access, like any other channel-scoped kind, and is
   served newest first (`created_at DESC, id ASC`) with the usual `until`
   cursor. Clients build a page library from that window: a page's newest
   revision is newer than all its others, so any page with a revision in the
   window has its head in the window. A client that stops paging at a bound
   MUST tell the reader the list may omit older pages.

## Write scope

Pages are shared channel documents, the same shape as channel canvas, so they
use canvas's write scope: **`channels:write`** for all three kinds. A token
narrowed to `messages:write` (a chat-only bot) does not gain the right to
rewrite shared documents by accident. Sessions without a scoped token, which is
how the desktop app, the CLI and managed agents authenticate, hold every scope
and are unaffected. Relaxing this later (for example letting `messages:write`
tokens publish suggestions only) is additive; tightening it would not be.

## Accepting a suggestion

A suggestion carries full replacement content, so applying it overwrites
whatever the page holds. To keep that safe:

- A suggestion can be accepted only while its `base` equals the current head.
  If the head has moved, the suggestion is **stale**: clients show it as out of
  date and the resolver rejects it or publishes a fresh suggestion against the
  new head. The relay rejects a revision whose `suggestion` tag names a
  suggestion with a different `base` than the revision's `prev`.
- The acceptance is **one event**: a `PAGE_REVISION` carrying
  `["suggestion", "<id>"]`. A suggestion is closed when either a resolution
  references it or a stored revision carries its `suggestion` tag. An
  `accepted` resolution is therefore optional audit; a lost resolution cannot
  leave an applied suggestion looking open.
- A `rejected` resolution by any writer closes the suggestion.
- A suggestion whose content already equals the head's content is already
  applied; it is closed with a `rejected` resolution (a no-op revision cannot
  be stored, by rule 4).
- A `suggestion` tag on a revision is rejected if it names an already-closed
  suggestion, or one not in the revision's `(h, d)`. A revision that carries it
  also carries `prev`.
- **Resolutions.** `e` must name a suggestion of the same `(h, d)`. `rejected`
  closes an open suggestion and carries no `rev`. `accepted` carries a `rev` that
  is a revision of the same `(h, d)` published by the resolver, and if that
  revision has a `suggestion` tag it names this suggestion. Closing is
  once-only: a second resolution of a suggestion that already has one is
  `conflict: suggestion is already resolved`; `rejected` after a revision applied
  the suggestion is `conflict: suggestion is already applied`; `accepted` after
  a revision applied it is allowed once, as audit, and then only with the
  applying revision as `rev`.
- A deleted resolution or revision no longer closes its suggestion.
- The relay serializes the closure check and the write per page, so applying and
  rejecting one suggestion at the same moment cannot both succeed.

## Reading

Everything is read with ordinary NIP-01 filters (REQ, COUNT, and the HTTP
`/query` and `/count` bridges), scoped to the channels the reader can access.
Results are newest first (`created_at` descending, ties by id).

| Read | Filter |
|------|--------|
| History of one page | `{"kinds":[52000], "#h":["<channel>"], "#d":["<page>"]}` |
| Revisions, suggestions and resolutions of one page | `{"kinds":[52000,52001,52002], "#h":["<channel>"], "#d":["<page>"]}` |
| Suggestions of a page and how they were resolved | `{"kinds":[52001,52002], "#h":["<channel>"], "#d":["<page>"]}`; a suggestion is also closed by a revision's `suggestion` tag, so read the history too |
| Library of a channel | `{"kinds":[52000], "#h":["<channel>"]}`; a page's head is its newest revision |
| Library across channels | `{"kinds":[52000]}`; scoped to the reader's channels (rule 9), paged with `until` set to the oldest `created_at` received (a boundary event can repeat; dedupe by id) |
| Number of revisions | the history filter in `COUNT` |
| Live updates | the same filters in a REQ subscription; new events fan out like any channel event |

A REQ registers its live subscription before it reads history, and the relay
fans an event out just after it answers the writer, so an event stored a moment
ago can arrive twice before EOSE (as history and as a live event). Clients
dedupe by event id.

A page's head is the revision no other revision names as `prev`; clients read it
as the newest revision, or follow `prev` for a fork. There is no server-side
library listing yet: the relay's `pages` index is internal.

## Errors

A rejected event is answered `["OK", id, false, "<message>"]` (HTTP `POST /events`
answers 400 with the same message). The prefix is the machine-readable part;
clients map `conflict:` to "refetch and retry" (CLI exit code 5).

| Message | Meaning |
|---------|---------|
| `conflict: stale prev (head <id>)` | `prev` is not the head; rebuild on `<id>` |
| `conflict: page already exists (head <id>)` | a first revision of an existing page |
| `conflict: prev revision not found` | `prev` was never stored (or was deleted, or lives in a channel the author cannot read) |
| `conflict: page does not exist` / `conflict: page is deleted` | no live page to advance |
| `conflict: suggestion is stale (its base is not the revision's prev)` | the head moved past the suggestion's `base` |
| `conflict: suggestion is already closed` | applying a suggestion that a resolution or revision closed |
| `conflict: suggestion is already resolved` / `... already applied` / `... already applied by another revision` | double-close of a suggestion |
| `conflict: page is busy with another writer; retry` | page writer lock not obtained in 5 s |
| `invalid: no-op revision (title and content equal the page head)` | rule 4 |
| `invalid: page content exceeds maximum size of 65536 bytes (got <n>)` | rule 5 |
| `invalid: page title must not be blank` / `invalid: page title exceeds 256 bytes` | rule 5 |
| `invalid: channel-scoped events must include an h tag` | no `h` tag (generic channel gate) |
| `invalid: page event must carry exactly one <tag> tag` / `... at most one prev tag` | tag multiplicity |
| `invalid: page <tag> tag is malformed: <reason>` | non-canonical UUID or event id |
| `invalid: <tag> event not found` | `base`, `e`, `suggestion` or `rev` names no stored event, or one in a channel the author cannot read |
| `invalid: <tag> event belongs to a different channel` / `... different page` | rule 2 (a different channel only when the author can read it) |
| `invalid: <tag> must reference a page revision event` / `... suggestion event` | wrong kind |
| `invalid: rev must be a revision published by the resolver` / `invalid: rev applies a different suggestion` | resolution `rev` rules |
| `invalid: channel is archived` | rule 1 |
| `restricted: not a channel member` | rule 1, private channel |

## Rebuild Invariant

The relay's page index (head pointer, title, timestamps) is a projection. It
MUST be reconstructable by replaying `PAGE_REVISION` events alone, and the
rebuilt result MUST equal the live index. Because the conflict check runs only
at ingest, a replay can meet several tips (an import, a restore, or events from
another relay): the head is then the tip with the greatest `created_at`, ties
broken by the lowest event id. If the head is deleted, the head is its `prev`,
repeatedly, until a live revision is reached.

The projection of one page `(h, d)`, computed over all its stored revisions
including deleted ones (revisions that fail the tag rules above are skipped):

| Field | Value |
|-------|-------|
| head | the tip (a revision no other revision names as `prev`) with the greatest `created_at`, ties to the lowest event id; if deleted, its `prev`, repeatedly |
| title, `updated_by`, `updated_at` | the head revision's title, author and own `created_at` (event time, never relay wall-clock time) |
| `created_by`, `created_at` | the root revision's author and `created_at`; with several roots, the earliest, ties to the lowest event id |
| revision count | every stored revision of the page, deleted ones included |

A page with no live head has no index row. A page is deleted by soft-deleting
its revision, suggestion and resolution events in the same atomic operation as
its index tombstone, so a replay agrees: it finds no live revision and emits no
row. An index tombstone without deleted events would be resurrected by a replay
and is therefore not a valid deletion.

**Deleting individual page events.** A page event can be deleted with NIP-09
(kind 5, by its author) or kind 9005 (a channel admin). The relay deletes it and
re-projects the page in the same transaction: deleting the head makes its `prev`
the head, deleting the last live revision removes the page's row (its id can be
created anew), and deleting a suggestion or resolution reopens or removes it
without touching the head. The index therefore never names a deleted event. A
deletion is a side effect of the stored deletion event; if it fails the page is
left exactly as it was and the failure is logged (as for every deletion kind).
Who may delete a whole page, as one operation, remains an open question.

**Not deleted with their channel.** Deleting a channel does not rewrite page
events; the library excludes pages of deleted channels at read time.

## Search

P1 does not integrate pages with search. Until it does, page kinds follow the
relay's default full-text indexing, so superseded revisions and suggestions can
match NIP-50 queries. Returning head revisions only requires either excluding
page kinds from the generic index or page-aware filtering in the search path;
that choice is deferred (see the open questions).

## Privacy

Pages inherit channel visibility. Nothing in this NIP publishes page content
outside the channel.

## Agents and the review gate

"Agents suggest, humans accept" is a client convention in P1: the CLI and the
ACP prompt steer agents to `suggest`, and the desktop UI makes acceptance an
explicit human action. The relay does not distinguish agent npubs from human
ones, so any channel writer can publish a revision directly. Enforcing the gate
at the protocol level needs a trusted agent-role signal and is an open question.

## Open Questions

- Protocol-level enforcement of the agent review gate (requires an agent-role
  signal the relay can trust).
- Whole-page deletion: who may do it, and as which event. (Deleting individual
  page events is defined under Rebuild Invariant.)
- Retention cap on stored revisions per page. The relay rejects no-op revisions
  and caps content at 64 KiB but does not prune history; a rebuild by replay
  refuses a page with more than 100,000 revisions rather than truncating it, so
  a cap or compaction must land before a page can grow that far.
- Search: exclude page kinds from the generic index vs page-aware filtering.
- A server-side library listing (the `pages` index is not yet readable over the
  wire; clients assemble the library from revisions).
- Optional opt-in NIP-23 publication for pages in public channels.

Resolved: **write scope** mirrors canvas (`channels:write`); see "Write scope".
