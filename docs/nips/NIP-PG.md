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

## Tags

Common: `["h", "<channel uuid>"]`, `["d", "<page id, uuid v4>"]`.

`PAGE_REVISION`
- `["prev", "<event id of the revision this was based on>"]` — omitted on the
  first revision of a page; required otherwise.
- `["title", "<utf-8 title>"]` — required, non-blank, at most 256 bytes.
- `["suggestion", "<event id>"]` — optional; present when the revision applies
  an accepted suggestion.
- `content`: markdown, UTF-8.

`PAGE_SUGGESTION`
- `["base", "<event id of the revision the suggestion edits>"]` — required.
- `content`: proposed full markdown. Clients derive the diff against `base`.

`PAGE_SUGGESTION_RESOLUTION`
- `["e", "<suggestion event id>"]`, `["status", "accepted" | "rejected"]`.
- `["rev", "<resulting revision id>"]` — required when accepted.

## Relay Rules

1. The author MUST be able to write to the channel; archived channels reject.
2. **Head.** A page's head is the newest revision in its `prev` chain. A new
   revision is accepted only if `prev` equals the current head (or the page does
   not exist and `prev` is absent). Otherwise the relay rejects with a
   `conflict:` message and stores nothing.
3. Revisions whose content and title equal the head's are rejected as no-ops.
4. Content is bounded to 64 KiB (the relay's `max_content_len`); oversize is
   rejected, not truncated.
5. A suggestion never changes the head. Any writer may publish one.
6. A resolution by a writer closes the suggestion for all viewers. An `accepted`
   resolution MUST reference a revision the resolver published.
7. Storing the event and advancing the head are one atomic operation.

## Rebuild Invariant

The relay's page index (head pointer, title, timestamps) is a projection. It
MUST be reconstructable by replaying `PAGE_REVISION` events alone. Operators can
therefore migrate relays or recover from index loss without data loss.

## Search

Only head revisions are indexed; superseded revisions are not returned.

## Privacy

Pages inherit channel visibility. Nothing in this NIP publishes page content
outside the channel.

## Open Questions

- Page deletion semantics (follow existing channel-event deletion).
- Retention cap on stored revisions per page.
- Write scope: mirror canvas scope or message scope.
- Optional opt-in NIP-23 publication for pages in public channels.
