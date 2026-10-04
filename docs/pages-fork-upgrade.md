# Pages: keeping this fork upgradeable

Pages are built in a private fork of `block/buzz`. The goal is that merging
upstream stays routine and never means rebuilding the feature.

## Rules

1. **Additive only.** Do not change the meaning of existing kinds, tables, or
   channel canvas behavior.
2. **New code in new files.** Shared files get one-line registrations only.
3. **Flagged.** Desktop UI sits behind the `pages` preview feature, off by
   default. The relay always accepts the new kinds.
4. **Kind block.** Pages use `52000–52099`. Upstream documents ranges only up to
   `49999`, and kinds must stay `<= 65535` because builders use
   `Kind::Custom(u16)`. Keep all page kinds in one commented block in
   `kind.rs` so a renumber is a single, mechanical edit.
5. **One migration, additive.** `CREATE ... IF NOT EXISTS`, no edits to existing
   tables, and the same change in `schema/schema.sql`. Upstream will add its
   own numbered migrations; on merge, renumber ours to the next free number.
6. **Replayable.** The page index must be rebuildable from events (NIP-PG).

## Conflict hotspots

| File | Our edit |
|------|----------|
| `crates/buzz-core/src/kind.rs` | one constants block plus `ALL_KINDS` entries |
| `crates/buzz-relay/src/handlers/ingest.rs` | scope and `h`-scope list entries |
| `migrations/`, `schema/schema.sql` | one additive file / block |
| `crates/buzz-cli/src/commands/mod.rs` | `pub mod pages;` and its dispatch |
| `desktop/src/shared/constants/kinds.ts` | mirrored kind constants |
| `mobile/lib/shared/relay/nostr_models.dart` | mirrored kind constants |
| desktop sidebar, routes, `e2eBridge.ts` | one entry each |
| `preview-features.json` | one `pages` entry |

Everything else lives in new files: `crates/buzz-db/src/store/page.rs`,
`crates/buzz-cli/src/commands/pages.rs`, `desktop/src/features/pages/`, and
`crates/buzz-test-client/tests/e2e_pages.rs`.

## Sync routine

```bash
git remote add upstream https://github.com/block/buzz   # once
git fetch upstream main
git merge upstream/main          # merge, not rebase
# resolve the hotspots above; renumber our migration if it collides
. ./bin/activate-hermit
just ci
just test                        # relay and db touched
# then the pages e2e suite
```

Sync often; small drift keeps hotspot conflicts to a line or two.

## After each merge, check

- No duplicate kind values (the `no_duplicate_kind_values` test).
- Fresh-database migration passes, and an upgrade test applying the old
  migration set and then ours passes.
- Pages e2e tests still bind the production ingest path (a guard removed from
  `ingest.rs` must still fail a test).
- Upstream has not allocated a kind or table name we use.
