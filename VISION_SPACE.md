# Vision: Space (Pages)

> Status: draft, fork-local. Protocol detail lives in [NIP-PG](docs/nips/NIP-PG.md);
> how this stays mergeable with upstream lives in
> [docs/pages-fork-upgrade.md](docs/pages-fork-upgrade.md).

## The Problem

A channel canvas is one markdown document with no history and no review.
Anything bigger — a plan, a runbook, meeting notes, a risk log — ends up in an
external tool that has no idea who is in the channel, and that agents cannot
edit safely. Humans and agents share a relay but not a document.

## What Space Is

**Pages** are channel-scoped documents with a revision history. **Space** is the
library surface that lists every page you can see across channels.

- A page belongs to a channel (`h` tag). Channel membership is the only gate.
- Every edit is a signed, append-only revision. History, attribution, and
  conflict detection come from the event log, not a side system.
- Agents are colleagues, not editors-in-chief: an agent **suggests** an edit, a
  human **accepts or rejects** it. Agents author with their own npub. In P1 this
  is a client convention (CLI, ACP prompt, desktop UI); the relay cannot yet
  tell agents from humans, so it is not protocol-enforced (see NIP-PG).
- The library is assembled at read time, like the Home feed, and only shows
  pages in channels the viewer can already read.

## Principles

- **Buzz is the pipe, not the brain.** The relay verifies, orders, stores, and
  fans out. It does not interpret page content.
- **Zero notifications by default.** Page edits and suggestions do not notify
  anyone unless a person opts in.
- **Verb, object, outcome.** Agent page activity reads as one sentence in the
  [activity feed](VISION_ACTIVITY.md): "Suggested an edit to *Q4 Plan* (+3/−1)
  → awaiting review."
- **Additive.** New kinds, new tables, new files. Nothing existing changes
  meaning, including channel canvas.
- **Portable.** Pages export to markdown, and the relay's page index is always
  rebuildable by replaying events.
- **Preview first.** Ships behind the `pages` preview feature, off by default.

## Surfaces

| Surface | Role |
|---------|------|
| **Space** (desktop) | Library of pages across channels; open, create, search. |
| **Page view** | Read, edit, history, pending suggestions with accept/reject. |
| **CLI** | `buzz pages ls/get/set/suggest/accept/reject/history/export`. |
| **Agents** | Read and suggest through the CLI; the ACP prompt teaches the verbs. |

## First Slice (P1)

In: multiple pages per channel, revision log with base-revision conflict check,
agent suggestions, CLI, desktop library and page view, markdown export.

Out: live co-editing, rich blocks, inline comments, canvas migration, NIP-23
publishing, web and mobile editing, an MCP connector for external assistants.

## Relationship to Canvas

Canvas is untouched in P1. A later change may re-express a channel canvas as its
home page; that is a separate, reversible migration.

## Deliberately Open

- Revision retention: reject no-op revisions and cap retained history (limit to
  be set with the relay change).
- Whether write access mirrors canvas scope or message scope.
- Protocol-level enforcement of "agents suggest, humans accept".
- Search: head revisions only (index exclusion vs page-aware filtering).
- Public publishing of selected pages as NIP-23 notes (public channels only).
- Real-time co-editing (a CRDT such as Yjs, using Tiptap's collaboration
  extension rather than custom code).
