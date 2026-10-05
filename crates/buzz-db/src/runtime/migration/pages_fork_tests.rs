//! Proofs for the fork-private pages migrations (`migrations/9001_pages_index.sql`
//! and `migrations/9002_pages_search.sql`).
//!
//! The fork rule (docs/pages-fork-upgrade.md, rule 5) is that an applied sqlx
//! migration is never renamed or edited, so the pages migrations take versions
//! far above upstream's sequence and an upstream merge can keep adding 0045,
//! 0046, ... around them. These tests pin the properties that make that safe:
//! the numbers, the additive-only shape, a fresh install, an upgrade from an
//! upstream-only database, a database that already applied them surviving a
//! later upstream migration, and the failure that renaming would cause.

use super::MIGRATOR;

/// Versions at or above this are fork-private; upstream's sequence stays below.
pub(super) const FORK_PRIVATE_VERSION_FLOOR: i64 = 9000;
/// The pages migration. Never change this number: it is recorded in every
/// database that has applied the migration.
const PAGES_MIGRATION_VERSION: i64 = 9001;
/// The page search projection (`pages.search_tsv`). Same rule: never change it.
const PAGES_SEARCH_MIGRATION_VERSION: i64 = 9002;
/// Minimum gap kept between the highest upstream version and the pages
/// migration. If an upstream merge ever erodes it, this test says so long
/// before the sequences could collide.
const REQUIRED_HEADROOM: i64 = 1_000;

/// Bring a database that stopped at an upstream migration up to date, so tests
/// that validate a schema-wide manifest (the deletion catalog lists `pages`)
/// after a bounded run up to an upstream version see the fork-private tables
/// too. Goes through the production entry point, so the schema/destruction lock
/// contract (see `migration_execution_cannot_bypass_schema_destruction_lock`)
/// holds.
pub(super) async fn apply_fork_private_migrations(pool: &sqlx::PgPool) {
    super::run_migrations(pool)
        .await
        .expect("apply the fork-private migrations");
}

fn upstream_versions() -> Vec<i64> {
    let mut versions: Vec<i64> = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .filter(|version| *version < FORK_PRIVATE_VERSION_FLOOR)
        .collect();
    versions.sort_unstable();
    versions
}

#[test]
fn pages_migrations_are_the_only_fork_private_versions_and_sort_after_upstream() {
    let mut fork_private: Vec<i64> = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .filter(|version| *version >= FORK_PRIVATE_VERSION_FLOOR)
        .collect();
    fork_private.sort_unstable();
    assert_eq!(
        fork_private,
        vec![PAGES_MIGRATION_VERSION, PAGES_SEARCH_MIGRATION_VERSION],
        "the fork carries exactly two private migrations; allocate further ones from \
         {FORK_PRIVATE_VERSION_FLOOR} upward and update this test"
    );
    for (version, description) in [
        (PAGES_MIGRATION_VERSION, "pages index"),
        (PAGES_SEARCH_MIGRATION_VERSION, "pages search"),
    ] {
        let migration = MIGRATOR
            .iter()
            .find(|migration| migration.version == version)
            .expect("pages migration is embedded");
        assert_eq!(&*migration.description, description);
    }

    let upstream = upstream_versions();
    let highest_upstream = *upstream.last().expect("upstream migrations exist");
    assert!(
        PAGES_MIGRATION_VERSION - highest_upstream >= REQUIRED_HEADROOM,
        "upstream reached {highest_upstream}; the fork-private version \
         {PAGES_MIGRATION_VERSION} is too close to collide-proof"
    );
}

#[test]
fn pages_migration_is_additive_and_idempotent_by_construction() {
    let sql = MIGRATOR
        .iter()
        .find(|migration| migration.version == PAGES_MIGRATION_VERSION)
        .expect("pages migration is embedded")
        .sql
        .as_ref()
        .to_owned();
    let code: String = sql
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<Vec<_>>()
        .join("\n");
    let statements: Vec<String> = code
        .split(';')
        .map(|statement| {
            statement
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase()
        })
        .filter(|statement| !statement.is_empty())
        .collect();
    assert!(!statements.is_empty());
    for statement in &statements {
        let allowed = statement.starts_with("create table if not exists pages ")
            || statement.starts_with("create index if not exists idx_pages_")
            || statement == "select attach_community_write_fence('pages')";
        assert!(
            allowed,
            "the pages migration may only create its own objects idempotently; found: {statement}"
        );
    }
    for forbidden in [
        " alter ",
        " drop ",
        " update ",
        " delete ",
        " insert ",
        " truncate ",
    ] {
        assert!(
            !format!(" {code} ").to_ascii_lowercase().contains(forbidden),
            "the pages migration must not edit existing objects or data ({forbidden:?})"
        );
    }
}

/// Split migration SQL into normalized (lowercase, single-spaced) statements,
/// ignoring `--` comments and keeping `$$ ... $$` bodies (whose inner `;`
/// belong to the function) inside their statement.
fn normalized_statements(sql: &str) -> Vec<String> {
    let code: String = sql
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<Vec<_>>()
        .join("\n");
    let mut statements = Vec::new();
    let mut current = String::new();
    for (index, part) in code.split("$$").enumerate() {
        if index % 2 == 1 {
            // Inside a dollar-quoted body: keep it out of the statement text.
            current.push_str(" $$body$$ ");
            continue;
        }
        let mut pieces = part.split(';').peekable();
        while let Some(piece) = pieces.next() {
            current.push_str(piece);
            if pieces.peek().is_some() {
                statements.push(std::mem::take(&mut current));
            }
        }
    }
    statements.push(current);
    statements
        .into_iter()
        .map(|statement| {
            statement
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase()
        })
        .filter(|statement| !statement.is_empty())
        .collect()
}

#[test]
fn pages_search_migration_is_additive_and_idempotent_by_construction() {
    let sql = MIGRATOR
        .iter()
        .find(|migration| migration.version == PAGES_SEARCH_MIGRATION_VERSION)
        .expect("pages search migration is embedded")
        .sql
        .as_ref()
        .to_owned();
    let statements = normalized_statements(&sql);
    assert_eq!(
        statements.len(),
        5,
        "column, function, trigger, index, backfill: {statements:?}"
    );
    for statement in &statements {
        let allowed = statement
            .starts_with("alter table pages add column if not exists search_tsv tsvector")
            || statement.starts_with("create or replace function pages_refresh_search_tsv() ")
            || statement.starts_with("create or replace trigger pages_search_tsv ")
            || statement.starts_with("create index if not exists idx_pages_search_tsv ")
            || statement
                == "update pages set head_event_id = head_event_id where search_tsv is null";
        assert!(
            allowed,
            "the page search migration may only extend `pages` idempotently; found: {statement}"
        );
    }
    // It never touches another table (the function reads `events`, never writes).
    let code = sql
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden in [
        " drop ",
        " truncate ",
        " delete ",
        "insert into",
        "alter table events",
        "update events",
    ] {
        assert!(
            !format!(" {code} ").contains(forbidden),
            "the page search migration must not edit existing objects ({forbidden:?})"
        );
    }
}

#[cfg(test)]
mod postgres_tests {
    use sqlx::migrate::{MigrateError, Migration, MigrationType, Migrator};
    use sqlx::{AssertSqlSafe, Connection, PgConnection, PgPool, SqlSafeStr};

    use super::super::run_migrations;
    use super::*;

    /// A throwaway database on the test server, created from `template0`.
    struct Scratch {
        admin: PgPool,
        name: String,
        url: String,
        pool: PgPool,
    }

    impl Scratch {
        async fn new(label: &str) -> Self {
            let base_url = crate::test_support::database_url();
            let admin = PgPool::connect(&base_url).await.expect("connect admin");
            let (url_prefix, _) = base_url.rsplit_once('/').expect("database url has a path");
            let name = format!("buzz_pages_{label}_{}", uuid::Uuid::new_v4().simple());
            sqlx::query(AssertSqlSafe(format!(
                "CREATE DATABASE {name} TEMPLATE template0"
            )))
            .execute(&admin)
            .await
            .expect("create scratch database");
            let url = format!("{url_prefix}/{name}");
            let pool = PgPool::connect(&url)
                .await
                .expect("connect scratch database");
            Self {
                admin,
                name,
                url,
                pool,
            }
        }

        /// Run `migrator` on a dedicated connection that is closed afterwards.
        ///
        /// sqlx takes a session-level advisory lock for the run and does not
        /// release it when validation fails, so a failing run must not hand its
        /// connection back to a pool: the next run would wait on that lock
        /// forever. (Production startup closes the connection the same way.)
        ///
        /// This runs a runtime-built `Migrator` (the embedded set plus a
        /// synthetic upstream migration) on a throwaway database that holds no
        /// tenant data, so the schema/destruction lock that
        /// `run_migrations` takes for live databases has nothing to protect.
        async fn run(&self, migrator: &Migrator) -> Result<(), MigrateError> {
            let mut conn = PgConnection::connect(&self.url)
                .await
                .expect("connect migration session");
            let outcome = migrator.run(&mut conn).await;
            let _ = conn.close().await;
            outcome
        }

        async fn finish(self) {
            self.pool.close().await;
            sqlx::query(AssertSqlSafe(format!(
                "DROP DATABASE {} WITH (FORCE)",
                self.name
            )))
            .execute(&self.admin)
            .await
            .expect("drop scratch database");
        }
    }

    async fn applied_versions(pool: &PgPool) -> Vec<i64> {
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(pool)
            .await
            .expect("read applied migrations")
    }

    async fn table_exists(pool: &PgPool, table: &str) -> bool {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(table)
        .fetch_one(pool)
        .await
        .expect("check table")
    }

    fn pages_migration() -> Migration {
        MIGRATOR
            .iter()
            .find(|migration| migration.version == PAGES_MIGRATION_VERSION)
            .expect("pages migration is embedded")
            .clone()
    }

    fn pages_search_migration() -> Migration {
        MIGRATOR
            .iter()
            .find(|migration| migration.version == PAGES_SEARCH_MIGRATION_VERSION)
            .expect("pages search migration is embedded")
            .clone()
    }

    /// The embedded migrations plus `extra`, as a freshly built migrator.
    fn migrator_with(extra: Vec<Migration>, skip: Option<i64>) -> Migrator {
        let mut migrations: Vec<Migration> = MIGRATOR
            .iter()
            .filter(|migration| Some(migration.version) != skip)
            .cloned()
            .collect();
        migrations.extend(extra);
        Migrator::with_migrations(migrations)
    }

    /// What an upstream merge would add: a migration numbered above every
    /// existing upstream one, but far below the fork-private version.
    fn synthetic_upstream_migration() -> Migration {
        let next = upstream_versions().last().copied().expect("upstream") + 1;
        Migration::new(
            next,
            "synthetic upstream".into(),
            MigrationType::Simple,
            "CREATE TABLE synthetic_upstream_marker (id INT PRIMARY KEY)".into_sql_str(),
            false,
        )
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn migration_schema_pages_fresh_database_applies_the_fork_private_migration() {
        let db = Scratch::new("fresh").await;
        run_migrations(&db.pool).await.expect("run migrations");

        let mut expected: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        expected.sort_unstable();
        assert_eq!(applied_versions(&db.pool).await, expected);
        assert!(expected.contains(&PAGES_MIGRATION_VERSION));
        assert!(expected.contains(&PAGES_SEARCH_MIGRATION_VERSION));
        assert!(table_exists(&db.pool, "pages").await);
        let fenced: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_trigger \
             WHERE tgrelid = 'pages'::regclass AND tgname = 'community_write_fence_pages' \
               AND NOT tgisinternal",
        )
        .fetch_one(&db.pool)
        .await
        .expect("fence trigger");
        assert_eq!(
            fenced, 1,
            "the migration attaches the community write fence"
        );

        // Idempotent: replaying the migration's SQL, and restarting, change nothing.
        sqlx::raw_sql(pages_migration().sql)
            .execute(&db.pool)
            .await
            .expect("replaying the pages migration is a no-op");
        sqlx::raw_sql(pages_search_migration().sql)
            .execute(&db.pool)
            .await
            .expect("replaying the page search migration is a no-op");
        run_migrations(&db.pool).await.expect("restart");
        assert_eq!(applied_versions(&db.pool).await, expected);
        db.finish().await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn migration_schema_pages_upgrades_a_database_that_only_has_upstream_migrations() {
        let db = Scratch::new("upgrade").await;
        let highest_upstream = *upstream_versions().last().expect("upstream");
        super::super::run_migrations_through(&db.pool, highest_upstream)
            .await
            .expect("apply upstream migrations only");
        assert_eq!(applied_versions(&db.pool).await, upstream_versions());
        assert!(!table_exists(&db.pool, "pages").await);

        // The fork's binary starts against the pre-pages database and adds the table.
        run_migrations(&db.pool).await.expect("fork upgrade");
        let mut expected = upstream_versions();
        expected.push(PAGES_MIGRATION_VERSION);
        expected.push(PAGES_SEARCH_MIGRATION_VERSION);
        assert_eq!(applied_versions(&db.pool).await, expected);
        assert!(table_exists(&db.pool, "pages").await);
        db.finish().await;
    }

    /// A database that already applied 9001 (a fork binary from before page
    /// search) upgrades by applying only 9002, which backfills the search
    /// vector of the pages it already holds.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn migration_schema_pages_search_backfills_pages_written_before_9002() {
        let db = Scratch::new("search_backfill").await;
        let before_9002 = migrator_with(vec![], Some(PAGES_SEARCH_MIGRATION_VERSION));
        db.run(&before_9002).await.expect("startup before 9002");
        assert!(!column_exists(&db.pool, "pages", "search_tsv").await);

        let community = uuid::Uuid::new_v4();
        let channel = uuid::Uuid::new_v4();
        let head = vec![9_u8; 32];
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(community)
            .bind(format!("backfill-{}.example", community.simple()))
            .execute(&db.pool)
            .await
            .expect("community");
        sqlx::query(
            "INSERT INTO channels (id, community_id, name, created_by) VALUES ($1, $2, 'c', $3)",
        )
        .bind(community)
        .bind(channel)
        .bind(vec![1_u8; 32])
        .execute(&db.pool)
        .await
        .expect("channel");
        sqlx::query(
            "INSERT INTO events (community_id, id, pubkey, created_at, kind, tags, content, sig, channel_id) \
             VALUES ($1, $2, $3, '2026-02-01T00:00:00Z', 52000, '[]'::jsonb, 'backfilledneedle body', $4, $5)",
        )
        .bind(community)
        .bind(&head)
        .bind(vec![2_u8; 32])
        .bind(vec![3_u8; 64])
        .bind(channel)
        .execute(&db.pool)
        .await
        .expect("head event");
        sqlx::query(
            "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
             created_by, created_at, updated_by, updated_at) \
             VALUES ($1, $2, $3, $4, 'Old page', $5, '2026-02-01T00:00:00Z', $5, '2026-02-01T00:00:00Z')",
        )
        .bind(community)
        .bind(channel)
        .bind(uuid::Uuid::new_v4())
        .bind(&head)
        .bind(vec![2_u8; 32])
        .execute(&db.pool)
        .await
        .expect("page written before 9002");

        run_migrations(&db.pool).await.expect("apply 9002");
        let matches: bool = sqlx::query_scalar(
            "SELECT search_tsv @@ plainto_tsquery('simple', 'backfilledneedle') AND \
                    search_tsv @@ plainto_tsquery('simple', 'page') \
             FROM pages WHERE community_id = $1",
        )
        .bind(community)
        .fetch_one(&db.pool)
        .await
        .expect("read backfilled vector");
        assert!(matches, "9002 projects the head of every existing page");
        db.finish().await;
    }

    async fn column_exists(pool: &PgPool, table: &str, column: &str) -> bool {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2)",
        )
        .bind(table)
        .bind(column)
        .fetch_one(pool)
        .await
        .expect("column lookup")
    }

    /// Proof (b): a database that already applied the pages migration starts
    /// cleanly once an upstream merge adds a later-numbered migration, and a
    /// fresh database applies the merged set with the pages migration last.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn migration_schema_pages_survives_a_later_upstream_migration() {
        let synthetic = synthetic_upstream_migration();
        let synthetic_version = synthetic.version;
        let merged = migrator_with(vec![synthetic], None);

        let db = Scratch::new("merge").await;
        // 1. The fork's binary before the upstream merge: upstream + pages.
        run_migrations(&db.pool).await.expect("pre-merge startup");
        let community = uuid::Uuid::new_v4();
        let channel = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO communities (id, host) VALUES ($1, $2)")
            .bind(community)
            .bind(format!("merge-{}.example", community.simple()))
            .execute(&db.pool)
            .await
            .expect("community");
        sqlx::query(
            "INSERT INTO channels (id, community_id, name, created_by) VALUES ($1, $2, 'c', $3)",
        )
        .bind(channel)
        .bind(community)
        .bind(vec![1_u8; 32])
        .execute(&db.pool)
        .await
        .expect("channel");
        sqlx::query(
            "INSERT INTO pages (community_id, channel_id, page_id, head_event_id, title, \
             created_by, created_at, updated_by, updated_at) \
             VALUES ($1, $2, $3, $4, 'kept', $5, NOW(), $5, NOW())",
        )
        .bind(community)
        .bind(channel)
        .bind(uuid::Uuid::new_v4())
        .bind(vec![7_u8; 32])
        .bind(vec![8_u8; 32])
        .execute(&db.pool)
        .await
        .expect("page row");

        // 2. The binary after the merge knows one more upstream migration and
        // the same pages migration: it applies only the new one.
        db.run(&merged).await.expect("post-merge startup");
        let mut expected = upstream_versions();
        expected.push(synthetic_version);
        expected.push(PAGES_MIGRATION_VERSION);
        expected.push(PAGES_SEARCH_MIGRATION_VERSION);
        assert_eq!(applied_versions(&db.pool).await, expected);
        assert!(table_exists(&db.pool, "synthetic_upstream_marker").await);
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM pages WHERE title = 'kept'")
            .fetch_one(&db.pool)
            .await
            .expect("page row survives");
        assert_eq!(kept, 1);
        // 3. And a further restart is a no-op.
        db.run(&merged).await.expect("restart after merge");
        assert_eq!(applied_versions(&db.pool).await, expected);
        db.finish().await;

        // A brand-new database applies the merged set; the pages migration, the
        // highest version, goes last.
        let fresh = Scratch::new("merge_fresh").await;
        fresh.run(&merged).await.expect("fresh merged install");
        assert_eq!(applied_versions(&fresh.pool).await, expected);
        let order: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY installed_on")
                .fetch_all(&fresh.pool)
                .await
                .expect("apply order");
        assert_eq!(
            order, expected,
            "migrations apply in ascending version order"
        );
        fresh.finish().await;
    }

    /// Why rule 5 exists: once applied, renaming (or editing) the migration is
    /// fatal at startup. This is the failure the fork-private number avoids.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn migration_schema_pages_renaming_or_editing_an_applied_migration_breaks_startup() {
        let db = Scratch::new("rename").await;
        run_migrations(&db.pool).await.expect("initial startup");

        let original = pages_migration();
        let renamed_version = synthetic_upstream_migration().version;
        let renamed = Migration::new(
            renamed_version,
            original.description.clone(),
            MigrationType::Simple,
            original.sql.clone(),
            false,
        );
        let after_rename = migrator_with(vec![renamed], Some(PAGES_MIGRATION_VERSION));
        assert!(
            matches!(
                db.run(&after_rename).await,
                Err(MigrateError::VersionMissing(PAGES_MIGRATION_VERSION))
            ),
            "a renamed migration leaves the applied version missing"
        );

        let edited = Migration::new(
            PAGES_MIGRATION_VERSION,
            original.description.clone(),
            MigrationType::Simple,
            AssertSqlSafe(format!(
                "{}\n-- edited after apply\n",
                original.sql.as_ref()
            ))
            .into_sql_str(),
            false,
        );
        let after_edit = migrator_with(vec![edited], Some(PAGES_MIGRATION_VERSION));
        assert!(
            matches!(
                db.run(&after_edit).await,
                Err(MigrateError::VersionMismatch(PAGES_MIGRATION_VERSION))
            ),
            "an edited migration fails its checksum"
        );
        db.finish().await;
    }

    struct PgConn {
        host: String,
        port: String,
        user: String,
        password: String,
    }

    fn pg_conn(url: &str) -> PgConn {
        let options: sqlx::postgres::PgConnectOptions = url.parse().expect("parse database url");
        let password = url
            .split_once("://")
            .and_then(|(_, rest)| rest.split_once('@'))
            .and_then(|(authority, _)| authority.split_once(':'))
            .map(|(_, password)| password.to_owned())
            .or_else(|| std::env::var("PGPASSWORD").ok())
            .unwrap_or_default();
        PgConn {
            host: options.get_host().to_owned(),
            port: options.get_port().to_string(),
            user: options.get_username().to_owned(),
            password,
        }
    }

    async fn catalog_of_pages(pool: &PgPool) -> Vec<String> {
        let mut lines = Vec::new();
        let columns: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT column_name, data_type, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = 'pages' ORDER BY column_name",
        )
        .fetch_all(pool)
        .await
        .expect("columns");
        lines.extend(columns.into_iter().map(|c| format!("column {c:?}")));
        let indexes: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT c.relname, pg_get_indexdef(i.indexrelid), i.indoption::int2[]::text \
             FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
             WHERE i.indrelid = 'pages'::regclass ORDER BY c.relname",
        )
        .fetch_all(pool)
        .await
        .expect("indexes");
        lines.extend(indexes.into_iter().map(|i| format!("index {i:?}")));
        let constraints: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT conname, contype::text, pg_get_constraintdef(oid) FROM pg_constraint \
             WHERE conrelid = 'pages'::regclass ORDER BY conname",
        )
        .fetch_all(pool)
        .await
        .expect("constraints");
        lines.extend(constraints.into_iter().map(|c| format!("constraint {c:?}")));
        let triggers: Vec<(String, String)> = sqlx::query_as(
            "SELECT tgname, pg_get_triggerdef(oid) FROM pg_trigger \
             WHERE tgrelid = 'pages'::regclass AND NOT tgisinternal ORDER BY tgname",
        )
        .fetch_all(pool)
        .await
        .expect("triggers");
        lines.extend(triggers.into_iter().map(|t| format!("trigger {t:?}")));
        lines
    }

    async fn fenced_tables(pool: &PgPool) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT c.relname::text FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
             JOIN pg_proc p ON p.oid = t.tgfoid \
             WHERE p.proname = 'enforce_community_write_fence' AND NOT t.tgisinternal \
               AND NOT c.relispartition ORDER BY c.relname",
        )
        .fetch_all(pool)
        .await
        .expect("fenced tables")
    }

    /// `schema/schema.sql` (the desired state fresh installs bootstrap from
    /// through the real `bin/pgschema`) and the migration must build the same
    /// `pages` table, and fence the same set of tables.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn migration_schema_pages_desired_state_matches_the_migrated_catalog() {
        let desired = Scratch::new("desired").await;
        let migrated = Scratch::new("migrated").await;
        let conn = pg_conn(&crate::test_support::database_url());
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let apply = std::process::Command::new(manifest.join("../../bin/pgschema"))
            .args([
                "apply",
                "--auto-approve",
                "--file",
                manifest
                    .join("../../schema/schema.sql")
                    .to_str()
                    .expect("schema path is utf-8"),
                "--host",
                &conn.host,
                "--port",
                &conn.port,
                "--user",
                &conn.user,
                "--password",
                &conn.password,
                "--db",
                &desired.name,
                "--plan-host",
                &conn.host,
                "--plan-port",
                &conn.port,
                "--plan-user",
                &conn.user,
                "--plan-password",
                &conn.password,
                "--plan-db",
                &desired.name,
            ])
            .output()
            .expect("run bin/pgschema apply (hermit environment required)");
        assert!(
            apply.status.success(),
            "pgschema apply failed: {}\n{}",
            String::from_utf8_lossy(&apply.stdout),
            String::from_utf8_lossy(&apply.stderr)
        );
        // pgschema does not preserve everything schema.sql declares; every apply
        // caller runs the reconciliation script right after it.
        let reconcile = std::fs::read_to_string(
            manifest.join("../../scripts/reconcile-schema-after-pgschema.sql"),
        )
        .expect("read reconciliation script");
        sqlx::raw_sql(AssertSqlSafe(reconcile))
            .execute(&desired.pool)
            .await
            .expect("reconcile desired-state schema");
        run_migrations(&migrated.pool)
            .await
            .expect("run migrations");

        let from_schema = catalog_of_pages(&desired.pool).await;
        assert!(!from_schema.is_empty(), "schema.sql must define pages");
        assert_eq!(
            from_schema,
            catalog_of_pages(&migrated.pool).await,
            "schema.sql and migration 9001 must describe the same pages table"
        );
        let schema_fences = fenced_tables(&desired.pool).await;
        assert!(schema_fences.contains(&"pages".to_owned()));
        assert_eq!(
            schema_fences,
            fenced_tables(&migrated.pool).await,
            "the desired-state and migrated schemas must fence the same tables"
        );
        desired.finish().await;
        migrated.finish().await;
    }
}
