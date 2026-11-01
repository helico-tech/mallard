//! DuckLake schema. `$S` is replaced by `<lake_alias>.<schema>`. See `docs/data-model.md` for the reasoning.
//!
//! All tables are append-only, except retention deletes on `events`.
//! Mutable state (issue status) is an append-only activity log where the
//! latest row wins. That keeps writes cheap in a lakehouse, where an
//! UPDATE means writing delete files.

/// Bump when the schema changes and add a migration step in `migrate`.
pub const SCHEMA_VERSION: i32 = 1;

pub const CREATE: &[&str] = &[
    "CREATE SCHEMA IF NOT EXISTS $S",
    "CREATE TABLE IF NOT EXISTS $S.meta (key VARCHAR NOT NULL, value VARCHAR NOT NULL, updated_at TIMESTAMP NOT NULL)",
    "CREATE TABLE IF NOT EXISTS $S.projects (
        id INTEGER NOT NULL,
        slug VARCHAR NOT NULL,
        name VARCHAR NOT NULL,
        platform VARCHAR,
        public_key VARCHAR NOT NULL,
        created_at TIMESTAMP NOT NULL
    )",
    // One row per issue, written once when the first event of a group arrives.
    "CREATE TABLE IF NOT EXISTS $S.issues (
        project_id INTEGER NOT NULL,
        group_hash UBIGINT NOT NULL,
        first_seen TIMESTAMP NOT NULL,
        first_event_id UUID NOT NULL,
        first_release VARCHAR,
        title VARCHAR NOT NULL,
        culprit VARCHAR,
        level VARCHAR NOT NULL,
        platform VARCHAR NOT NULL,
        exception_type VARCHAR
    ) SORTED BY (project_id, group_hash)",
    // Status changes; the latest row per issue is the current status.
    "CREATE TABLE IF NOT EXISTS $S.issue_activity (
        project_id INTEGER NOT NULL,
        group_hash UBIGINT NOT NULL,
        changed_at TIMESTAMP NOT NULL,
        status VARCHAR NOT NULL,
        reason VARCHAR NOT NULL
    ) SORTED BY (project_id, group_hash)",
    // The fact table. Daily partitions make retention a file drop and
    // time-window queries prune whole days; sorting by (project, group, time)
    // inside each file gives tight min/max stats so per-issue queries skip
    // most row groups.
    "CREATE TABLE IF NOT EXISTS $S.events (
        project_id INTEGER NOT NULL,
        received_at TIMESTAMP NOT NULL,
        timestamp TIMESTAMP NOT NULL,
        event_id UUID NOT NULL,
        group_hash UBIGINT NOT NULL,
        level VARCHAR NOT NULL,
        platform VARCHAR NOT NULL,
        environment VARCHAR,
        release VARCHAR,
        dist VARCHAR,
        server_name VARCHAR,
        transaction_name VARCHAR,
        logger VARCHAR,
        title VARCHAR NOT NULL,
        culprit VARCHAR,
        exception_type VARCHAR,
        exception_value VARCHAR,
        message VARCHAR,
        handled BOOLEAN,
        user_key VARCHAR,
        trace_id UUID,
        span_id UBIGINT,
        sdk_name VARCHAR,
        sdk_version VARCHAR,
        tags MAP(VARCHAR, VARCHAR),
        size_bytes INTEGER NOT NULL,
        payload VARCHAR NOT NULL
    ) PARTITIONED BY (epoch_day(received_at)) SORTED BY (project_id, group_hash, received_at)",
    // Sourcemap artifact bundles (one row per bundle and project).
    "CREATE TABLE IF NOT EXISTS $S.artifact_bundles (
        bundle_id VARCHAR NOT NULL,
        project_id INTEGER NOT NULL,
        release VARCHAR,
        dist VARCHAR,
        object_key VARCHAR NOT NULL,
        size_bytes BIGINT NOT NULL,
        file_count INTEGER NOT NULL,
        created_at TIMESTAMP NOT NULL
    )",
    // Files inside bundles, for debug-id and release+url lookups.
    "CREATE TABLE IF NOT EXISTS $S.artifact_files (
        bundle_id VARCHAR NOT NULL,
        project_id INTEGER NOT NULL,
        debug_id UUID,
        url VARCHAR,
        file_type VARCHAR NOT NULL,
        zip_path VARCHAR NOT NULL,
        sourcemap_url VARCHAR,
        release VARCHAR,
        dist VARCHAR
    ) SORTED BY (project_id, debug_id)",
];

/// Table options applied after creation (idempotent).
/// Batches up to this many events are inlined into the catalog; bigger ones
/// (only under heavy load) are written straight to Parquet.
pub const EVENTS_INLINE_ROWS: usize = 1000;

pub fn table_options() -> Vec<String> {
    vec![
        // Small batches stay inlined in the catalog until a flush (by time or
        // row count) writes them out as one well-sorted Parquet file per day.
        format!("ALTER TABLE $S.events SET (data_inlining_row_limit = {EVENTS_INLINE_ROWS})"),
        "ALTER TABLE $S.events SET (parquet_compression = 'zstd')".into(),
        // Payloads are large and never filtered on: no min/max stats.
        "ALTER TABLE $S.events SET (skip_stats_columns = ['payload', 'message', 'exception_value'])".into(),
        "ALTER TABLE $S.events SET (target_file_size = '256MB')".into(),
        "ALTER TABLE $S.issues SET (data_inlining_row_limit = 10000)".into(),
        "ALTER TABLE $S.issue_activity SET (data_inlining_row_limit = 10000)".into(),
        "ALTER TABLE $S.artifact_files SET (data_inlining_row_limit = 10000)".into(),
    ]
}
