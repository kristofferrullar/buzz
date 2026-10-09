# Pages: keeping this fork upgradeable

Pages are built in a private fork of `block/buzz`. The goal is that merging
upstream stays routine and never means rebuilding the feature.

## Rules

1. **Additive only.** Do not change the meaning of existing kinds, tables, or
   channel canvas behavior.
2. **New code in new files.** Shared files get one-line registrations only.
3. **Flagged.** Desktop UI sits behind the `pages` preview feature, off by
   default. From the relay change (PR3) on, the relay always accepts the new
   kinds; until then it rejects them as unknown.
4. **Kind block.** Pages use `52000–52099`. Upstream documents ranges only up to
   `49999`, and kinds must stay `<= 65535` because builders use
   `Kind::Custom(u16)`. Keep all page kinds in one commented block in
   `kind.rs`. A renumber is that block plus the two client mirrors; a parity
   test fails if `kinds.ts` or `nostr_models.dart` drift from `kind.rs`.
5. **Few migrations, additive, never renumbered.** `CREATE ... IF NOT EXISTS`, no
   edits to upstream tables (in particular never `events` or its generated
   `search_tsv`), and the same change in `schema/schema.sql`. sqlx records each
   applied migration by version and checksum, so renaming or editing a
   migration after it has been applied fails startup. The pages migrations are
   therefore **`migrations/9001_pages_index.sql`** (the `pages` table) and
   **`migrations/9002_pages_search.sql`** (the search projection on that same
   table: `pages.search_tsv`, its GIN index and the trigger that maintains it):
   fork-private versions are allocated from 9000 upward, far above upstream's
   sequence (0001-0044 today), so an upstream merge never forces a rename.

   What was proven (tests in
   `crates/buzz-db/src/runtime/migration/pages_fork_tests.rs`, all passing):
   - sqlx (0.9) applies any pending version in ascending order and checks only
     that every *applied* version is still embedded and unchanged. It does not
     require contiguous numbers and does not reject a pending version lower than
     the highest applied one. No repo script, CI job or lint requires
     contiguous numbering. `ignore_missing` is not needed and is not used.
   - A fresh database applies 0001-00NN and then 9001 and 9002 last; an
     upstream-only database upgrades by applying just those two; both migrations
     are idempotent; a database that applied only 9001 upgrades by applying 9002,
     which backfills the search vector of the pages it already holds; 9002 is
     pinned to its additive shape (one column, one function, one trigger, one
     index, one backfill, all on `pages`).
   - A database that already applied 9001 and 9002 starts cleanly after a later
     upstream migration is added (simulated with a synthetic migration numbered
     above upstream's highest, built in the test, not committed), and a fresh
     database applies the merged set with 9001 and 9002 last.
   - Renaming 9001 fails with `VersionMissing(9001)` and editing it fails with
     `VersionMismatch(9001)`: the failures this rule exists to avoid.
   - `schema/schema.sql` and the migration build the same `pages` table and fence
     the same tables (the desired state is bootstrapped through the real
     `bin/pgschema`, then `scripts/reconcile-schema-after-pgschema.sql`).
   - A test fails if upstream's highest version ever comes within 1,000 of 9001.

   Consequences to remember: tests that count or index upstream migrations must
   exclude versions >= 9000 (`embedded_migrator_contains_consolidated_initial_schema`
   does), and a community-scoped table needs its row in the deletion manifests
   below.
6. **Replayable.** The page index must be rebuildable from events (NIP-PG).

## Conflict hotspots

| File | Our edit |
|------|----------|
| `crates/buzz-core/src/kind.rs` | one constants block plus `ALL_KINDS` entries |
| `crates/buzz-relay/src/handlers/ingest.rs` | scope arm and `h`-scope list entries; two hooks (`pages::validate_shape` after the other envelope validators, `pages::store_page_event` as the first storage branch); a test checks every page kind in `ALL_KINDS` appears in both lists |
| `crates/buzz-relay/src/handlers/req.rs` | `stores_d_tag` (NIP-33 or page kind) replaces the NIP-33-only test in `filter_to_query_params` and `filter_fully_pushable`, so `#d` on page kinds is pushed into SQL (REQ, COUNT, HTTP bridges) |
| `crates/buzz-relay/src/handlers/side_effects.rs` | two 4-line hooks in the 9005 and NIP-09 deletion handlers calling `pages::delete_page_event` (delete a page event and repair the head in one transaction) |
| `crates/buzz-relay/src/handlers/mod.rs` | `pub mod pages;` |
| `crates/buzz-db/src/store/event.rs` | `extract_d_tag` also returns the page id for page kinds (the `d_tag` column the `#d` pushdown reads) |
| `.github/workflows/_ci-relay.yml` | `--test e2e_pages` in the Relay E2E step, and one `cargo test -p buzz-cli --test pages_cli -- --ignored` line beside it |
| `migrations/`, `schema/schema.sql` | two additive files (`9001_pages_index.sql`, `9002_pages_search.sql`) / one block above the deletion section (the table, its `search_tsv` column, GIN index, function and trigger) |
| `crates/buzz-search/src/query.rs` | `search()` keeps its statement; four edits: `AND kind NOT IN (page kinds)` after the base `WHERE` (`page_head::push_page_kind_exclusion`), `created_at` added to the select list (so the `UNION ALL` result can order by it), the scope-filter block extracted unchanged into `push_event_filters` (both arms share it), and one `page_head::push_page_head_arm` call before `ORDER BY`; `push_tsquery` is `pub(crate)` |
| `crates/buzz-search/src/lib.rs` | `mod page_head;` |
| `crates/buzz-db/src/store/deletion.rs` | `"pages"` in `EXPECTED_SCOPED_TABLES` and, before `"channels"`, in `PURGE_SCOPED_TABLES` (a community-scoped table missing from the first blocks community deletion) |
| `crates/buzz-db/src/runtime/migration.rs` | one `mod pages_fork_tests;` line; the `< FORK_PRIVATE_VERSION_FLOOR` filter in `embedded_migrator_contains_consolidated_initial_schema`; one `apply_fork_private_migrations` call before the catalog check in `migration_0044_drops_populated_nip_fi_ledger_cleanly` (that test stops at 0044 and the deletion manifest now lists `pages`) |
| `crates/buzz-db/src/store/mod.rs`, `crates/buzz-db/src/lib.rs` | `pub mod page;` and `pub use store::page;` |
| `crates/buzz-cli/src/commands/mod.rs`, `crates/buzz-cli/src/lib.rs` | `pub mod pages;`; in `lib.rs` one `Cmd::Pages(commands::pages::PagesCmd)` variant, one dispatch arm, and the `pages` entries in the `command_inventory_is_stable` / `subcommand_counts_are_stable` tests (the `PagesCmd` enum itself lives in `commands/pages.rs`) |
| `crates/buzz-acp/src/base_prompt.md`, `crates/buzz-acp/src/lib.rs` | a `buzz pages` row in the CLI table, `5 write conflict` in the exit-code list, and one `## Pages` paragraph (pinned by `base_prompt_pages.rs`, registered with one `#[cfg(test)] mod base_prompt_pages;` line) |
| `desktop/src-tauri/src/managed_agents/nest_skill.md`, `.../nest.rs` | the Pages workflow section and `pages` output-contract rows in the agent skill (`.claude/skills/sprout-cli/SKILL.md` is a symlink to this file); `NEST_SKILL_VERSION` bumped so installed agents refresh it |
| `desktop/src/shared/constants/kinds.ts` | mirrored kind constants |
| `mobile/lib/shared/relay/nostr_models.dart` | mirrored kind constants |
| desktop sidebar, routes, `e2eBridge.ts` | one entry each |
| `preview-features.json` | one `pages` entry |

Everything else lives in new files: `crates/buzz-db/src/store/page.rs` (with its
ingest primitives and tests under `crates/buzz-db/src/store/page/`),
`crates/buzz-relay/src/handlers/pages.rs` (validation and atomic storage),
`crates/buzz-search/src/page_head.rs` (page-aware search: the exclusion and the
head arm, with `crates/buzz-search/tests/postgres_pages_search.rs`),
`crates/buzz-cli/src/commands/pages.rs` (with `pages/model.rs` and `pages/tests.rs`),
`crates/buzz-cli/tests/pages_cli.rs`, `desktop/src/features/pages/`, and
`crates/buzz-test-client/tests/e2e_pages.rs`. The agent-facing text (the `pages`
block in `crates/buzz-acp/src/base_prompt.md` and the Pages section of the
`sprout-cli` skill) is additive and stands or falls with the CLI.

## Sync routine

```bash
git remote add upstream https://github.com/block/buzz   # once
git fetch upstream main
git merge upstream/main          # merge, not rebase
# resolve the hotspots above (our migration keeps its version; see rule 5)
. ./bin/activate-hermit
just ci
just test                        # relay and db touched
# then the pages e2e suite
```

Sync often; small drift keeps hotspot conflicts to a line or two.

## After each merge, check

- No duplicate kind values (the `no_duplicate_kind_values` test).
- Fresh-database migration passes, and an upgrade test applying our migration
  first and upstream's new ones afterwards (a database that already applied
  ours) passes: `cargo nextest run -p buzz-db pages_fork_tests` (Postgres lane).
- `EXPECTED_SCOPED_TABLES` still equals the live set of community-scoped tables
  (the deletion catalog tests), and `pages` is still fenced.
- Pages e2e tests still bind the production ingest path (a guard removed from
  `ingest.rs` must still fail a test).
- `#d` on page kinds is still answered in SQL: `stores_d_tag` in `req.rs` and
  `extract_d_tag` in `event.rs` must both still cover page kinds
  (`quiet_page_history_is_exact_in_a_flooded_channel` fails if either is lost).
- Page search is still head-only: `search()` in `query.rs` still excludes page
  kinds from the generic arm and calls `push_page_head_arm`, and the scope
  filters still go through `push_event_filters` (`postgres_pages_search.rs`
  fails if any of the three is lost). If upstream changes the FTS policy of
  `events.search_tsv` (allowlist or exclusion list), nothing here depends on it
  (`postgres_pages_search.rs` runs every scenario under both policies).
- Upstream has not started storing a different value in `events.d_tag` for
  regular kinds, and no upstream query treats `d_tag IS NOT NULL` as "NIP-33 row".
- Upstream has not allocated a kind or table name we use.
- `NEST_SKILL_VERSION` in `nest.rs` is still above the version upstream shipped
  before our skill text (a merge that lands upstream's own bump at the same
  number would stop installed agents refreshing the Pages section), and the
  `Pages Workflow` section survived the merge of `nest_skill.md`
  (`cargo test -p buzz-cli --test pages_cli` pins the text).
