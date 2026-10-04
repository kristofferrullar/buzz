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

Common: `["h", "<channel uuid>"]`, `["d", "<page id, uuid v4>"]`.

`PAGE_REVISION`
- `["prev", "<event id of the revision this was based on>"]` — omitted on the
  first revision of a page; required otherwise.
- `["title", "<utf-8 title>"]` — required, non-blank, at most 256 bytes.
- `["suggestion", "<event id>"]` — optional; present when the revision applies
  a suggestion (see Accepting a suggestion).
- `content`: markdown, UTF-8.

`PAGE_SUGGESTION`
- `["base", "<event id of the revision the suggestion edits>"]` — required.
- `content`: proposed full markdown. Clients derive the diff against `base`.

`PAGE_SUGGESTION_RESOLUTION`
- `["e", "<suggestion event id>"]`, `["status", "accepted" | "rejected"]`.
- `["rev", "<resulting revision id>"]` — required when accepted.

## Relay Rules

1. The author MUST be able to write to the channel; archived channels reject.
2. **Scope.** Every page event MUST carry exactly one `h` tag and one `d` tag,
   and every referenced event MUST exist and share the event's `(h, d)`.
3. **Head.** A page's head is the newest revision in its `prev` chain. A new
   revision is accepted only if `prev` equals the current head (or the page does
   not exist and `prev` is absent). Otherwise the relay rejects with a
   `conflict:` message and stores nothing.
4. Revisions whose content and title equal the head's are rejected as no-ops.
5. Content is bounded to 64 KiB for page kinds; oversize is rejected, not
   truncated. (The relay's generic event cap is larger; page kinds are held to
   this tighter limit by a page-specific check.)
6. A suggestion never changes the head. Any writer may publish one.
7. Storing the event and advancing the head are one atomic operation.
8. **Queryability.** `#h` plus `#d` page queries MUST be answered exactly, not
   by filtering after a row limit, so a quiet page's history is complete in a
   busy channel.

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
  suggestion, or one not in the revision's `(h, d)`.

## Rebuild Invariant

The relay's page index (head pointer, title, timestamps) is a projection. It
MUST be reconstructable by replaying `PAGE_REVISION` events alone, and the
rebuilt result MUST equal the live index. Because the conflict check runs only
at ingest, a replay can meet several tips (an import, a restore, or events from
another relay): the head is then the tip with the greatest `created_at`, ties
broken by the lowest event id. If the head is deleted, the head is its `prev`.

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
- Page deletion semantics (follow existing channel-event deletion).
- Retention cap on stored revisions per page.
- Write scope: mirror canvas scope or message scope.
- Search: exclude page kinds from the generic index vs page-aware filtering.
- Optional opt-in NIP-23 publication for pages in public channels.
