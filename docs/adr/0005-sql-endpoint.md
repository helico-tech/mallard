# 0005. Read-only SQL endpoint, guarded by DuckDB's parser

Date: 2026-11-01

## Status

accepted

## Context

Ad-hoc SQL is the most flexible way to get insights (`odl sql "..."`). But
DuckDB SQL can read local files (`read_csv('/etc/passwd')`,
`FROM 'file.parquet'`), call maintenance functions that delete data
(`ducklake_cleanup_old_files`), and read settings. Running it in the server
process needs care.

## Decision

`POST /api/v1/sql` is **off by default**. It can only be enabled when API
tokens are configured. Each query is checked before execution:

1. `json_serialize_sql()`: DuckDB's own parser must accept it as exactly one
   `SELECT` statement (everything else is rejected by the serializer).
2. Walk the AST: table references must be lake tables (optionally qualified
   as `lake.<schema>.`) or CTEs defined in the query. That blocks replacement
   scans on file paths.
3. Table functions must be on an allowlist (`range`, `generate_series`,
   `unnest`, `json_each`, `json_tree`).
4. Scalar functions with risky prefixes (`getenv`, `current_setting`,
   `read_`, `ducklake`, `duckdb_`, ...) are rejected.

Results are limited (default 1000 rows), and the query timeout applies.

## Consequences

- Defense in depth, not a sandbox. Treat SQL access as admin access, and only
  give the token to trusted users.
- New DuckDB functions with side effects would need to be added to the
  denylist; the table-function allowlist fails closed.
