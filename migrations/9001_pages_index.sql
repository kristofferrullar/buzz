-- Pages head index (NIP-PG, fork-private).
--
-- FORK-PRIVATE VERSION. This file is deliberately numbered 9001, far above the
-- upstream sequence (0001-0044 at the time of writing). sqlx records every
-- applied migration by (version, checksum) and refuses to start when an applied
-- version is missing from the embedded set, so this file must NEVER be renamed
-- or edited once applied anywhere. A high version means an upstream merge can
-- add 0045, 0046, ... without ever colliding with it. sqlx applies any pending
-- version regardless of order, so a database that already applied 9001 still
-- applies a later upstream 0045 cleanly, and a fresh database applies 0001..00NN
-- first and 9001 last. See docs/pages-fork-upgrade.md (rule 5). This migration
-- depends only on `communities`, `channels` and attach_community_write_fence()
-- (0001 and 0029), never on anything newer, so that ordering difference is
-- harmless.
--
-- The `pages` table is a PROJECTION of the append-only PAGE_REVISION events
-- (kind 52000): one row per page, naming its current head revision. It is
-- rebuildable by replaying those events (NIP-PG "Rebuild Invariant"), so it
-- carries no information the event log does not.
--
-- A page is identified by (channel_id, page_id), the NIP-PG `(h, d)` pair: the
-- same page id in two channels is two pages. Tenant scoping follows the rest of
-- the schema: community_id is NOT NULL and leads the primary key and every index.
--
-- Additive only: CREATE ... IF NOT EXISTS, no edits to existing objects.

CREATE TABLE IF NOT EXISTS pages (
    community_id    UUID NOT NULL REFERENCES communities(id),
    channel_id      UUID NOT NULL,
    page_id         UUID NOT NULL,
    -- Head revision event id. Not a foreign key: `events` is partitioned and has
    -- no unique key on id alone.
    head_event_id   BYTEA NOT NULL CHECK (length(head_event_id) = 32),
    -- Title of the head revision (NIP-PG: non-blank, at most 256 bytes).
    title           TEXT NOT NULL CHECK (length(btrim(title)) > 0 AND octet_length(title) <= 256),
    -- Author and timestamp of the page's first revision.
    created_by      BYTEA NOT NULL CHECK (length(created_by) = 32),
    created_at      TIMESTAMPTZ NOT NULL,
    -- Author and event timestamp of the head revision. updated_at is the head
    -- event's own created_at (not wall-clock time) so a replay reproduces it.
    updated_by      BYTEA NOT NULL CHECK (length(updated_by) = 32),
    updated_at      TIMESTAMPTZ NOT NULL,
    -- Revisions accepted into the page's log. Soft-deleted revisions still count.
    revision_count  INT NOT NULL DEFAULT 1 CHECK (revision_count >= 1),
    -- Soft delete. A tombstoned page is hidden from every read and rejects
    -- further revisions; the page's events are soft-deleted in the same
    -- transaction so a replay agrees.
    deleted_at      TIMESTAMPTZ,
    PRIMARY KEY (community_id, channel_id, page_id),
    FOREIGN KEY (community_id, channel_id) REFERENCES channels (community_id, id)
);

-- Library listing: newest-updated first across a set of channels, keyset
-- paginated on (updated_at, channel_id, page_id).
CREATE INDEX IF NOT EXISTS idx_pages_library
    ON pages (community_id, updated_at DESC, channel_id DESC, page_id DESC)
    WHERE deleted_at IS NULL;

-- Universal community write fence (0029). The dynamic attach loop in 0029 ran
-- long before this table existed, so attach explicitly. Idempotent.
SELECT attach_community_write_fence('pages');
