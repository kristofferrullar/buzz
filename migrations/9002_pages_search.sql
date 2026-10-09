-- Page search index (NIP-PG "Search", fork-private).
--
-- FORK-PRIVATE VERSION, allocated from 9000 upward like 9001 (see
-- 9001_pages_index.sql and docs/pages-fork-upgrade.md, rule 5): never rename or
-- edit this file once it has been applied anywhere. It depends only on `pages`
-- (9001) and `events` (0001), never on anything newer.
--
-- Why this lives on `pages` and not on `events`: `events.search_tsv` is a
-- generated column whose kind policy differs by how a database was built (a
-- migrated database indexes only an allowlist of kinds and no page kind; a
-- database built from schema.sql or upgraded in place indexes every kind it
-- does not exclude, i.e. every revision, suggestion and resolution). Fork rule
-- 1 forbids changing that column, and no policy over `events` can give
-- head-only matching. The head revision is exactly what `pages` already names,
-- so its searchable text is projected here, next to the head pointer:
--
--   search_tsv = title (weight A) || content of the head revision event
--
-- The projection is maintained by a BEFORE trigger on the head columns, so it
-- is correct for every writer of `pages` (create, advance, delete-repair and
-- rebuild-by-replay) without any of them knowing about search. Like the rest of
-- `pages` it is derived data: a replay of the PAGE_REVISION events reproduces
-- it. The head event is found by id AND timestamp: `updated_at` is the head
-- event's own `created_at` (NIP-PG rebuild invariant), which also lets the
-- lookup touch a single partition of `events`. A page whose head event cannot
-- be found (or is deleted, or is not a revision) gets a NULL vector, which
-- never matches: search fails closed.
--
-- Additive only: one new column, one function, one trigger and one index on the
-- fork's own table, all idempotent. No existing object is edited.

ALTER TABLE pages ADD COLUMN IF NOT EXISTS search_tsv TSVECTOR;

CREATE OR REPLACE FUNCTION pages_refresh_search_tsv() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.search_tsv := (
        SELECT setweight(to_tsvector('simple', NEW.title), 'A')
               || to_tsvector('simple', e.content)
          FROM events e
         WHERE e.community_id = NEW.community_id
           AND e.id = NEW.head_event_id
           AND e.created_at = NEW.updated_at
           AND e.kind = 52000
           AND e.deleted_at IS NULL
         LIMIT 1
    );
    RETURN NEW;
END
$$;

CREATE OR REPLACE TRIGGER pages_search_tsv
    BEFORE INSERT OR UPDATE OF head_event_id, title ON pages
    FOR EACH ROW EXECUTE FUNCTION pages_refresh_search_tsv();

CREATE INDEX IF NOT EXISTS idx_pages_search_tsv ON pages USING GIN (search_tsv);

-- Backfill pages written before this migration (fires the trigger; the value
-- of head_event_id is unchanged).
UPDATE pages SET head_event_id = head_event_id WHERE search_tsv IS NULL;
