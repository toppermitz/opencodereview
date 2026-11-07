//! Schema migrations, run once at startup on a connection of their own. Every instance runs this;
//! a session advisory lock makes them take turns, and each re-reads what is applied after it gets
//! the lock, so a second instance finds the work done.

use super::conn::Error;
use super::pool::DbSettings;

/// Ordered migrations; `schema_migrations` records which have run. Append new ones to the end and
/// never edit one that has shipped.
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("0001_baseline.sql")),
    (2, include_str!("0002_scratch.sql")),
    (3, include_str!("0003_json_functions.sql")),
    (4, include_str!("0004_scratch_access.sql")),
    (5, include_str!("0005_archive.sql")),
    (6, include_str!("0006_instances.sql")),
    (7, include_str!("0007_result_publication_owner.sql")),
    (8, include_str!("0008_index_progress.sql")),
    (9, include_str!("0009_usage_attempts.sql")),
    (10, include_str!("0010_purge_merged_archives_immediately.sql")),
    (11, include_str!("0011_grok_provider.sql")),
];

/// Arbitrary, fixed key for `pg_advisory_lock`; shared by every instance of this application.
const MIGRATION_LOCK_KEY: i64 = 0x6f63_725f_6d69_6772; // "ocr_migr"

pub fn migrate(settings: &DbSettings) -> Result<(), String> {
    let mut client = settings.connect_owner().map_err(|error| error.to_string())?;
    migrate_client(&mut client)
}

pub(crate) fn migrate_client(client: &mut postgres::Client) -> Result<(), String> {
    client
        .execute("SELECT pg_advisory_lock($1)", &[&MIGRATION_LOCK_KEY])
        .map_err(|error| Error::from(error).to_string())?;
    let result = apply_pending(client);
    let _ = client.execute("SELECT pg_advisory_unlock($1)", &[&MIGRATION_LOCK_KEY]);
    result
}

fn apply_pending(client: &mut postgres::Client) -> Result<(), String> {
    let db_error = |error: postgres::Error| Error::from(error).to_string();
    client
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
               version BIGINT PRIMARY KEY,
               applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
             )",
        )
        .map_err(db_error)?;
    let applied: Vec<i64> = client
        .query("SELECT version FROM schema_migrations ORDER BY version", &[])
        .map_err(db_error)?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let latest = MIGRATIONS.last().map_or(0, |(version, _)| *version);
    if let Some(newest) = applied.iter().max().filter(|newest| **newest > latest) {
        return Err(format!(
            "database schema {newest} is newer than this application supports ({latest})"
        ));
    }
    for (version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }
        let mut tx = client.transaction().map_err(db_error)?;
        tx.batch_execute(sql)
            .map_err(|error| format!("migration {version} failed: {}", Error::from(error)))?;
        tx.execute("INSERT INTO schema_migrations (version) VALUES ($1)", &[version])
            .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
    }
    Ok(())
}
